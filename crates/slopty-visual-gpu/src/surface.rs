//! Frames rendered straight into `IOSurface`-backed BGRA pixel buffers.
//!
//! A visual's last pass draws into a buffer from a CoreVideo pool, shown to wgpu as a render
//! target through a CoreVideo Metal texture cache; GPUI's surface (`Window::paint_surface`)
//! samples the same buffer where it lies. No frame is read back to the CPU and uploaded again
//! (`docs/decisions/ui.md`, "An animating visual renders into the surface GPUI shows").
//!
//! The pool hands an `IOSurface` out again, in a new pixel buffer, only once nothing holds the
//! last buffer it lay in: GPUI holds the one it shows until its frame left the GPU, so a frame
//! never draws over the picture on screen.

use std::ptr::NonNull;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_video::{
    CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer,
    CVPixelBufferCreate, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetIOSurface, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferPool,
    CVPixelBufferUnlockBaseAddress, CVReturn, kCVMetalTextureUsage, kCVPixelBufferHeightKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelFormatType_32BGRA,
    kCVReturnSuccess,
};
use objc2_io_surface::IOSurfaceID;
use objc2_metal::{MTLPixelFormat, MTLTextureType, MTLTextureUsage};

/// One rendered frame: an `IOSurface`-backed pixel buffer of straight-alpha BGRA, top row
/// first, which GPUI's surface draws from without a copy.
#[derive(Clone)]
pub struct Frame {
    buffer: CFRetained<CVPixelBuffer>,
}

#[expect(clippy::non_send_fields_in_send_ty, reason = "CVBuffer is documented thread-safe")]
// SAFETY: CVBuffer.h: "CVBuffers may be used from any thread". Retain and release are atomic,
// and a frame is never written once it is handed out: the GPU finished drawing it first, and
// everything after reads it.
unsafe impl Send for Frame {}
// SAFETY: as above; every use through `&Frame` reads.
unsafe impl Sync for Frame {}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("width", &self.width())
            .field("height", &self.height())
            .finish()
    }
}

/// A CoreVideo call's status as a visual's error.
fn check(call: &str, status: CVReturn) -> Result<(), String> {
    if status == kCVReturnSuccess { Ok(()) } else { Err(format!("{call} failed: {status}")) }
}

/// A count as a CoreFoundation number.
fn number(value: usize) -> CFRetained<CFNumber> {
    CFNumber::new_i64(i64::try_from(value).unwrap_or(i64::MAX))
}

/// The attributes of an `IOSurface`-backed BGRA buffer `width` × `height` Metal can render to.
fn attributes(width: usize, height: usize) -> CFRetained<CFDictionary<CFString, CFType>> {
    let none = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
    let format = CFNumber::new_i64(i64::from(kCVPixelFormatType_32BGRA));
    let (width, height) = (number(width), number(height));
    let metal = CFBoolean::new(true);
    // SAFETY: CVPixelBuffer.h's constant strings, which live forever.
    let keys = unsafe {
        [
            kCVPixelBufferIOSurfacePropertiesKey,
            kCVPixelBufferMetalCompatibilityKey,
            kCVPixelBufferPixelFormatTypeKey,
            kCVPixelBufferWidthKey,
            kCVPixelBufferHeightKey,
        ]
    };
    CFDictionary::<CFString, CFType>::from_slices(
        &keys,
        &[&*none, metal, &*format, &*width, &*height],
    )
}

/// `buffer`'s base address, locked for as long as this lives.
struct Locked<'a> {
    buffer: &'a CVPixelBuffer,
    flags: CVPixelBufferLockFlags,
}

impl<'a> Locked<'a> {
    fn new(buffer: &'a CVPixelBuffer, flags: CVPixelBufferLockFlags) -> Result<Self, String> {
        // SAFETY: CVPixelBuffer.h's rule for `CVPixelBufferLockBaseAddress`: a valid buffer,
        // unlocked with the same flags (in `drop`).
        let status = unsafe { CVPixelBufferLockBaseAddress(buffer, flags) };
        check("CVPixelBufferLockBaseAddress", status)?;
        Ok(Self { buffer, flags })
    }

    /// Each of the buffer's `height` rows of `stride` bytes.
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "the rows are the buffer's own bytes: one borrow of the lock at a time writes them"
    )]
    fn rows(&mut self) -> impl Iterator<Item = &mut [u8]> {
        let base = CVPixelBufferGetBaseAddress(self.buffer).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRow(self.buffer);
        let height = CVPixelBufferGetHeight(self.buffer);
        (0..height).map(move |y| {
            // SAFETY: CVPixelBuffer.h: a locked, non-planar buffer's bytes run `bytes per row ×
            // height` from its base address, so row `y` starts inside them.
            let row = unsafe { base.add(y.saturating_mul(stride)) };
            // SAFETY: as above, `stride` bytes from there are the buffer's, and the lock keeps
            // them there while `self` lives.
            unsafe { std::slice::from_raw_parts_mut(row, stride) }
        })
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: as in `new`: locked there with these flags.
        let _unlocked = unsafe { CVPixelBufferUnlockBaseAddress(self.buffer, self.flags) };
    }
}

impl Frame {
    /// Its width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        u32::try_from(CVPixelBufferGetWidth(&self.buffer)).unwrap_or(u32::MAX)
    }

    /// Its height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        u32::try_from(CVPixelBufferGetHeight(&self.buffer)).unwrap_or(u32::MAX)
    }

    /// The pixel buffer, as GPUI's `Window::paint_surface` takes it.
    #[must_use]
    pub fn pixel_buffer(&self) -> CFRetained<CVPixelBuffer> {
        self.buffer.clone()
    }

    /// The `IOSurface` its pixels lie in, which outlives the pixel buffer: a pool hands its few
    /// surfaces back frame after frame, each time in a new pixel buffer.
    #[must_use]
    pub fn surface_id(&self) -> Option<IOSurfaceID> {
        CVPixelBufferGetIOSurface(Some(&self.buffer)).map(|surface| surface.id())
    }

    /// A frame a CPU drew (the software scene renderer's): `bgra`, straight alpha, rows packed
    /// with no padding, copied once into a new `IOSurface`-backed buffer. `None` when `bgra`
    /// is not `width` × `height` pixels or CoreVideo makes no buffer.
    #[must_use]
    pub fn from_bgra(width: u32, height: u32, bgra: &[u8]) -> Option<Self> {
        let (width, height) = (width as usize, height as usize);
        let row = width.checked_mul(4)?;
        if width == 0 || height == 0 || row.checked_mul(height)? != bgra.len() {
            return None;
        }
        let attributes = attributes(width, height);
        let mut raw: *mut CVPixelBuffer = std::ptr::null_mut();
        // SAFETY: CVPixelBuffer.h's rule for `CVPixelBufferCreate`: a valid out-pointer and a
        // dictionary of `kCVPixelBuffer*` keys; the buffer comes back owned (+1).
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width,
                height,
                kCVPixelFormatType_32BGRA,
                Some(attributes.as_opaque()),
                NonNull::from(&mut raw),
            )
        };
        check("CVPixelBufferCreate", status).ok()?;
        // SAFETY: owned (+1), as above.
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
        {
            let mut locked = Locked::new(&buffer, CVPixelBufferLockFlags::empty()).ok()?;
            for (dst, src) in locked.rows().zip(bgra.chunks_exact(row)) {
                dst.get_mut(..row)?.copy_from_slice(src);
            }
        }
        Some(Self { buffer })
    }

    /// Its pixels, read back to the CPU as straight-alpha BGRA, rows packed with no padding:
    /// for tests and tools that look at a frame, never for showing one.
    #[must_use]
    pub fn read_pixels(&self) -> Vec<u8> {
        let row = (self.width() as usize).saturating_mul(4);
        let mut out = Vec::with_capacity(row.saturating_mul(self.height() as usize));
        if let Ok(mut locked) = Locked::new(&self.buffer, CVPixelBufferLockFlags::ReadOnly) {
            for src in locked.rows() {
                out.extend_from_slice(src.get(..row).unwrap_or_default());
            }
        }
        out
    }
}

/// Where one visual's frames are drawn: a pool of `IOSurface`-backed BGRA buffers of one size,
/// and the texture cache that shows each to the visual GPU as a render target.
pub(crate) struct Surfaces {
    size: (u32, u32),
    pool: CFRetained<CVPixelBufferPool>,
    cache: CFRetained<CVMetalTextureCache>,
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the pool is thread-safe, the cache unshared"
)]
// SAFETY: CVPixelBufferPool.h documents no thread affinity, and VideoToolbox's own pools are
// handed between threads. The texture cache is used by one thread at a time: a visual's state,
// which holds it, goes to the thread rendering its frame and comes back with the frame.
unsafe impl Send for Surfaces {}

impl std::fmt::Debug for Surfaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Surfaces").field("size", &self.size).finish_non_exhaustive()
    }
}

/// A pool buffer shown to wgpu as a render target, for one frame.
pub(crate) struct Target {
    buffer: CFRetained<CVPixelBuffer>,
    /// The cache's texture of the buffer, held until the GPU finished drawing into it.
    metal: CFRetained<CVMetalTexture>,
    texture: wgpu::Texture,
}

impl Target {
    /// The texture a frame's last pass draws into.
    pub(crate) const fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

impl Surfaces {
    /// Buffers `width` × `height` for frames `device` draws.
    pub(crate) fn new(device: &wgpu::Device, (width, height): (u32, u32)) -> Result<Self, String> {
        let attributes = attributes(width as usize, height as usize);
        let mut pool: *mut CVPixelBufferPool = std::ptr::null_mut();
        // SAFETY: CVPixelBufferPool.h's rule for `CVPixelBufferPoolCreate`: a valid out-pointer
        // and a dictionary of `kCVPixelBuffer*` keys; the pool comes back owned (+1).
        let status = unsafe {
            CVPixelBufferPool::create(
                None,
                None,
                Some(attributes.as_opaque()),
                NonNull::from(&mut pool),
            )
        };
        check("CVPixelBufferPoolCreate", status)?;
        let pool = NonNull::new(pool).ok_or("CVPixelBufferPoolCreate made no pool")?;
        // SAFETY: owned (+1), as above.
        let pool = unsafe { CFRetained::from_raw(pool) };
        // What the cache's textures may be used for: drawn into, and read.
        let usage = number((MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead).0);
        // SAFETY: CVMetalTexture.h's constant string, which lives forever.
        let usage_key = unsafe { kCVMetalTextureUsage };
        let texture_attributes =
            CFDictionary::<CFString, CFType>::from_slices(&[usage_key], &[&*usage]);
        // SAFETY: wgpu's rule for `Device::as_hal`: the Metal device is only borrowed here, to
        // make a texture cache that draws on it, and wgpu keeps it alive past every use (the
        // cache retains it too).
        let metal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }
            .ok_or("the visual GPU is not Metal")?;
        let mut cache: *mut CVMetalTextureCache = std::ptr::null_mut();
        // SAFETY: CVMetalTextureCache.h's rule for `CVMetalTextureCacheCreate`: a live Metal
        // device, a dictionary of `kCVMetalTexture*` keys and a valid out-pointer; the cache
        // comes back owned (+1).
        let status = unsafe {
            CVMetalTextureCache::create(
                None,
                None,
                metal.raw_device(),
                Some(texture_attributes.as_opaque()),
                NonNull::from(&mut cache),
            )
        };
        drop(metal);
        check("CVMetalTextureCacheCreate", status)?;
        let cache = NonNull::new(cache).ok_or("CVMetalTextureCacheCreate made no cache")?;
        // SAFETY: owned (+1), as above.
        let cache = unsafe { CFRetained::from_raw(cache) };
        Ok(Self { size: (width, height), pool, cache })
    }

    /// A free buffer from the pool, as a texture `device` draws into.
    pub(crate) fn next(&self, device: &wgpu::Device) -> Result<Target, String> {
        let mut buffer: *mut CVPixelBuffer = std::ptr::null_mut();
        // SAFETY: CVPixelBufferPool.h's rule for `CVPixelBufferPoolCreatePixelBuffer`: a live
        // pool and a valid out-pointer; the buffer comes back owned (+1).
        let status = unsafe {
            CVPixelBufferPool::create_pixel_buffer(None, &self.pool, NonNull::from(&mut buffer))
        };
        check("CVPixelBufferPoolCreatePixelBuffer", status)?;
        let buffer = NonNull::new(buffer).ok_or("the pool made no buffer")?;
        // SAFETY: owned (+1), as above.
        let buffer = unsafe { CFRetained::from_raw(buffer) };
        let (width, height) = self.size;
        let mut metal: *mut CVMetalTexture = std::ptr::null_mut();
        // SAFETY: CVMetalTextureCache.h's rule for
        // `CVMetalTextureCacheCreateTextureFromImage`: a live cache, an `IOSurface`-backed BGRA
        // buffer of this size, its only plane (0) viewed in its own format, and a valid
        // out-pointer; the texture comes back owned (+1).
        let status = unsafe {
            CVMetalTextureCache::create_texture_from_image(
                None,
                &self.cache,
                &buffer,
                None,
                MTLPixelFormat::BGRA8Unorm,
                width as usize,
                height as usize,
                0,
                NonNull::from(&mut metal),
            )
        };
        check("CVMetalTextureCacheCreateTextureFromImage", status)?;
        let metal = NonNull::new(metal).ok_or("the cache made no texture")?;
        // SAFETY: owned (+1), as above.
        let metal = unsafe { CFRetained::from_raw(metal) };
        let raw = CVMetalTextureGetTexture(&metal).ok_or("the cache's texture has no Metal one")?;
        let extent = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        // SAFETY: wgpu-hal's rule for `texture_from_raw`: a 2D, single-layer, single-level
        // BGRA8 texture of this size, made on the device wgpu draws with (the cache's).
        let hal = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                raw,
                crate::FORMAT,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent { width, height, depth: 1 },
                None,
            )
        };
        // SAFETY: wgpu's rule for `create_texture_from_hal`: the texture comes from this
        // device's Metal device and matches the descriptor; it starts uninitialized and is
        // never said to be cleared, and every frame clears it as its last pass begins.
        let texture = unsafe {
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                hal,
                &wgpu::TextureDescriptor {
                    label: Some("visual frame"),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: crate::FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                },
                wgpu::TextureUses::UNINITIALIZED,
                false,
            )
        };
        Ok(Target { buffer, metal, texture })
    }

    /// The frame `target` holds, once the GPU finished drawing it.
    pub(crate) fn finish(&self, target: Target) -> Frame {
        let Target { buffer, metal, texture } = target;
        drop(texture);
        drop(metal);
        // Lets the cache let go of what it holds of textures released since.
        self.cache.flush(0);
        Frame { buffer }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CPU's frame copies in row for row into an `IOSurface` GPUI's surface can sample, and
    /// reads back the same.
    #[test]
    fn a_cpu_frame_reads_back_as_written() {
        let bgra: Vec<u8> =
            (0..3 * 2 * 4).map(|i| u8::try_from(i * 7 % 256).unwrap_or(0)).collect();
        let frame = Frame::from_bgra(3, 2, &bgra).expect("a frame");
        assert_eq!((frame.width(), frame.height()), (3, 2));
        assert!(frame.surface_id().is_some(), "IOSurface-backed, as GPUI's surface needs");
        assert_eq!(frame.read_pixels(), bgra);
        assert!(Frame::from_bgra(3, 2, &bgra[4..]).is_none(), "too few pixels");
        assert!(Frame::from_bgra(0, 2, &[]).is_none(), "no pixels");
    }
}

//! `VTDecompressionSession` fed Annex B; rebuilds itself from in-band parameter sets.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag,
    kCMTimeInvalid,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, kVTDecompressionPropertyKey_RealTime,
};
use slopty_proto::screen::VideoCodec;

use crate::CodecError;
use crate::annexb::{self, h264, hevc};
use crate::cf::{self, check};

/// A decoded picture that may cross threads.
///
/// `CVPixelBuffer` is reference counted and immutable once the decoder hands it out, which is
/// what makes moving it to the render thread sound.
pub struct PixelBuffer(CFRetained<CVPixelBuffer>);

// SAFETY: CoreVideo buffers are thread-safe reference-counted objects (CVBuffer.h: "CVBuffers
// may be used from any thread"); decoded output is not written to after the callback.
#[expect(clippy::non_send_fields_in_send_ty, reason = "CVBuffer is documented thread-safe")]
unsafe impl Send for PixelBuffer {}
// SAFETY: as above; every accessor used through `&PixelBuffer` is a read.
unsafe impl Sync for PixelBuffer {}

impl PixelBuffer {
    /// Take ownership of a retained buffer.
    #[must_use]
    pub const fn from_retained(buffer: CFRetained<CVPixelBuffer>) -> Self {
        Self(buffer)
    }

    /// The underlying buffer.
    #[must_use]
    pub fn as_cv(&self) -> &CVPixelBuffer {
        &self.0
    }

    /// Raw `CVPixelBufferRef`, retained; the caller owns one reference.
    #[must_use]
    pub fn into_raw(self) -> NonNull<CVPixelBuffer> {
        CFRetained::into_raw(self.0)
    }

    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> usize {
        CVPixelBufferGetWidth(&self.0)
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> usize {
        CVPixelBufferGetHeight(&self.0)
    }
}

impl std::fmt::Debug for PixelBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelBuffer").field("w", &self.width()).field("h", &self.height()).finish()
    }
}

/// One decoded frame.
#[derive(Debug)]
pub struct DecodedFrame {
    /// The picture, IOSurface-backed and Metal-compatible.
    pub image: PixelBuffer,
    /// The presentation timestamp given to `decode`.
    pub pts_us: u64,
}

type Sink = Box<dyn Fn(DecodedFrame) + Send + Sync>;

struct Shared {
    sink: Sink,
}

/// A hardware decoder for one stream.
pub struct Decoder {
    codec: VideoCodec,
    session: Option<CFRetained<VTDecompressionSession>>,
    format: Option<CFRetained<CMFormatDescription>>,
    parameter_sets: Vec<Vec<u8>>,
    shared: Arc<Shared>,
    frames_in: u64,
}

// SAFETY: VideoToolbox sessions are documented as usable from any thread; the decode path
// is only ever driven by one caller at a time (`decode` takes `&mut self`) and the output
// callback arrives on a VideoToolbox thread. `Shared` is only touched through atomics and the
// `Send + Sync` sink.
#[expect(clippy::non_send_fields_in_send_ty, reason = "VTDecompressionSession is thread-safe")]
unsafe impl Send for Decoder {}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder")
            .field("codec", &self.codec)
            .field("ready", &self.session.is_some())
            .field("frames_in", &self.frames_in)
            .finish_non_exhaustive()
    }
}

impl Decoder {
    /// A decoder that delivers pictures to `sink` on VideoToolbox's thread.
    pub fn new(codec: VideoCodec, sink: impl Fn(DecodedFrame) + Send + Sync + 'static) -> Self {
        Self {
            codec,
            session: None,
            format: None,
            parameter_sets: Vec::new(),
            shared: Arc::new(Shared { sink: Box::new(sink) }),
            frames_in: 0,
        }
    }

    /// True once a keyframe with parameter sets has configured the session.
    #[must_use]
    pub const fn ready(&self) -> bool {
        self.session.is_some()
    }

    /// Frames submitted.
    #[must_use]
    pub const fn frames_in(&self) -> u64 {
        self.frames_in
    }

    /// Decode one Annex B access unit. Output arrives asynchronously through the sink.
    pub fn decode(&mut self, annexb: &[u8], pts_us: u64) -> Result<(), CodecError> {
        let is_ps: fn(&[u8]) -> bool = match self.codec {
            VideoCodec::Hevc | VideoCodec::HevcMain10 => hevc::is_parameter_set,
            VideoCodec::H264 => h264::is_parameter_set,
        };
        let sets: Vec<Vec<u8>> =
            annexb::nal_units(annexb).filter(|nal| is_ps(nal)).map(<[u8]>::to_vec).collect();
        if !sets.is_empty() && sets != self.parameter_sets {
            self.configure(&sets)?;
            self.parameter_sets = sets;
        }
        let Some(session) = self.session.as_ref() else {
            return Err(CodecError::NoParameterSets);
        };
        let Some(format) = self.format.as_ref() else {
            return Err(CodecError::NoParameterSets);
        };
        let body = annexb::annexb_to_length_prefixed(annexb, is_ps);
        if body.is_empty() {
            return Ok(());
        }
        let sample = sample_buffer(&body, format, cf::time_us(pts_us))?;
        let mut info = VTDecodeInfoFlags::empty();
        // SAFETY: the sample buffer is valid and owned by us; the session outlives the call.
        // Frames are returned through the output callback, so no source refcon is needed.
        let status = unsafe {
            session.decode_frame(
                &sample,
                VTDecodeFrameFlags::Frame_EnableAsynchronousDecompression,
                ptr::null_mut(),
                &raw mut info,
            )
        };
        check("VTDecompressionSessionDecodeFrame", status)?;
        self.frames_in = self.frames_in.saturating_add(1);
        Ok(())
    }

    /// Build a format description from parameter sets and (re)create the session.
    fn configure(&mut self, sets: &[Vec<u8>]) -> Result<(), CodecError> {
        let format = format_description(self.codec, sets)?;
        if let Some(session) = self.session.as_ref() {
            // SAFETY: both objects are valid for the call.
            if unsafe { session.can_accept_format_description(&format) } {
                self.format = Some(format);
                return Ok(());
            }
            // SAFETY: invalidating a live session is always allowed; it is dropped right after.
            unsafe { session.invalidate() }
            self.session = None;
        }
        let attrs = CFDictionary::<CFString, CFType>::from_slices(
            &[
                // SAFETY: framework-provided constant string.
                unsafe { kCVPixelBufferIOSurfacePropertiesKey },
                // SAFETY: framework-provided constant string.
                unsafe { kCVPixelBufferMetalCompatibilityKey },
            ],
            &[&CFDictionary::<CFString, CFType>::empty(), cf::boolean(true)],
        );
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(output_callback),
            decompressionOutputRefCon: Arc::as_ptr(&self.shared).cast_mut().cast::<c_void>(),
        };
        let mut raw: *mut VTDecompressionSession = ptr::null_mut();
        // SAFETY: all pointers are valid for the call; the refcon is an `Arc<Shared>` that this
        // `Decoder` keeps alive for as long as the session (see `Drop`).
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                None,
                Some(attrs.as_opaque()),
                &raw const record,
                NonNull::from(&mut raw),
            )
        };
        check("VTDecompressionSessionCreate", status)?;
        let Some(raw) = NonNull::new(raw) else {
            return Err(CodecError::Os { call: "VTDecompressionSessionCreate", status: -1 });
        };
        // SAFETY: `create` returned a +1 reference.
        let session = unsafe { CFRetained::from_raw(raw) };
        cf::set_property(
            &session,
            // SAFETY: framework-provided constant string.
            unsafe { kVTDecompressionPropertyKey_RealTime },
            cf::boolean(true),
            "VTSessionSetProperty(RealTime)",
        )?;
        self.session = Some(session);
        self.format = Some(format);
        Ok(())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            // SAFETY: invalidation stops callbacks; after it returns the session no longer
            // touches the refcon, so the `Arc<Shared>` may be released with `self`.
            unsafe { session.invalidate() }
        }
    }
}

unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    _source: *mut c_void,
    status: i32,
    flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    pts: CMTime,
    _duration: CMTime,
) {
    if status != 0 || flags.contains(VTDecodeInfoFlags::FrameDropped) {
        tracing::debug!(status, ?flags, "decoder dropped a frame");
        return;
    }
    let Some(image) = NonNull::new(image) else { return };
    // SAFETY: VideoToolbox hands the callback a borrowed image buffer; retaining it here gives
    // the sink its own reference.
    let image = unsafe { CFRetained::retain(image) };
    // SAFETY: the refcon was created from `Arc::as_ptr` on the decoder's `Shared`, which the
    // `Decoder` keeps alive until the session is invalidated (see `Drop`).
    let shared: &Shared = unsafe { &*refcon.cast::<Shared>() };
    let pts_us = cf::micros(pts).unwrap_or(0);
    (shared.sink)(DecodedFrame { image: PixelBuffer(image), pts_us });
}

fn format_description(
    codec: VideoCodec,
    sets: &[Vec<u8>],
) -> Result<CFRetained<CMFormatDescription>, CodecError> {
    let mut pointers: Vec<NonNull<u8>> = Vec::with_capacity(sets.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(sets.len());
    for set in sets {
        let Some(first) = set.first() else { continue };
        pointers.push(NonNull::from(first));
        sizes.push(set.len());
    }
    if pointers.is_empty() {
        return Err(CodecError::NoParameterSets);
    }
    let mut out: *const CMFormatDescription = ptr::null();
    let (call, status) = match codec {
        VideoCodec::Hevc | VideoCodec::HevcMain10 => (
            "CMVideoFormatDescriptionCreateFromHEVCParameterSets",
            // SAFETY: the pointer and size arrays have `pointers.len()` valid entries and the
            // parameter set bytes outlive the call.
            unsafe {
                CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    None,
                    pointers.len(),
                    NonNull::from(pointers.as_mut_slice()).cast::<NonNull<u8>>(),
                    NonNull::from(sizes.as_mut_slice()).cast::<usize>(),
                    4,
                    None,
                    NonNull::from(&mut out),
                )
            },
        ),
        VideoCodec::H264 => (
            "CMVideoFormatDescriptionCreateFromH264ParameterSets",
            // SAFETY: as above.
            unsafe {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    None,
                    pointers.len(),
                    NonNull::from(pointers.as_mut_slice()).cast::<NonNull<u8>>(),
                    NonNull::from(sizes.as_mut_slice()).cast::<usize>(),
                    4,
                    NonNull::from(&mut out),
                )
            },
        ),
    };
    check(call, status)?;
    let Some(out) = NonNull::new(out.cast_mut()) else {
        return Err(CodecError::Os { call, status: -1 });
    };
    // SAFETY: the create function returned a +1 reference.
    Ok(unsafe { CFRetained::from_raw(out) })
}

/// Wrap length-prefixed NAL units in a sample buffer the decoder accepts.
fn sample_buffer(
    body: &[u8],
    format: &CMFormatDescription,
    pts: CMTime,
) -> Result<CFRetained<CMSampleBuffer>, CodecError> {
    let mut block: *mut CMBlockBuffer = ptr::null_mut();
    // SAFETY: a NULL memory block asks CoreMedia to allocate `body.len()` bytes itself; the
    // out-pointer is valid.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            ptr::null_mut(),
            body.len(),
            None,
            ptr::null(),
            0,
            body.len(),
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut block),
        )
    };
    check("CMBlockBufferCreateWithMemoryBlock", status)?;
    let Some(block) = NonNull::new(block) else {
        return Err(CodecError::Os { call: "CMBlockBufferCreateWithMemoryBlock", status: -1 });
    };
    // SAFETY: +1 reference from the create call.
    let block = unsafe { CFRetained::from_raw(block) };
    if let Some(first) = body.first() {
        // SAFETY: the source has `body.len()` readable bytes and the block buffer was created
        // with exactly that capacity.
        let status = unsafe {
            CMBlockBuffer::replace_data_bytes(
                NonNull::from(first).cast::<c_void>(),
                &block,
                0,
                body.len(),
            )
        };
        check("CMBlockBufferReplaceDataBytes", status)?;
    }
    let timing = CMSampleTimingInfo {
        // SAFETY: framework-provided constant.
        duration: unsafe { kCMTimeInvalid },
        presentationTimeStamp: pts,
        // SAFETY: framework-provided constant.
        decodeTimeStamp: unsafe { kCMTimeInvalid },
    };
    let size = body.len();
    let mut sample: *mut CMSampleBuffer = ptr::null_mut();
    // SAFETY: one sample, one timing entry, one size entry; every pointer is valid for the call.
    let status = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(&block),
            Some(format),
            1,
            1,
            &raw const timing,
            1,
            &raw const size,
            NonNull::from(&mut sample),
        )
    };
    check("CMSampleBufferCreateReady", status)?;
    let Some(sample) = NonNull::new(sample) else {
        return Err(CodecError::Os { call: "CMSampleBufferCreateReady", status: -1 });
    };
    // SAFETY: +1 reference from the create call.
    Ok(unsafe { CFRetained::from_raw(sample) })
}

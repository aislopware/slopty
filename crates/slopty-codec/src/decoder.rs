//! `VTDecompressionSession` fed Annex B; rebuilds itself from in-band parameter sets, and
//! again from the next keyframe's after the system took the session away.

use std::ffi::{c_char, c_void};
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag,
    kCMTimeInvalid,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, kVTDecompressionPropertyKey_RealTime, kVTInvalidSessionErr,
    kVTVideoDecoderMalfunctionErr,
};
use slopty_proto::screen::VideoCodec;

use crate::CodecError;
use crate::annexb::{AccessUnit, h264, hevc};
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

/// A frame the decoder returned no picture for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DecodeFailure {
    /// The presentation timestamp given to `decode`.
    pub pts_us: u64,
    /// VideoToolbox's status; zero when it dropped the frame without naming an error.
    pub status: i32,
}

impl DecodeFailure {
    /// The system took the session away, so frames predicted from anything before are
    /// undecodable and only a keyframe restarts the stream (see [`session_lost`]).
    #[must_use]
    pub const fn session_lost(&self) -> bool {
        session_lost(self.status)
    }
}

/// What the decoder made of one submitted frame.
pub type DecodeOutcome = Result<DecodedFrame, DecodeFailure>;

/// Whether `status` means the system took the session away for good.
///
/// That is `kVTInvalidSessionErr` (an iOS app sent to the background, a Mac that slept,
/// `mediaserverd` restarting) or `kVTVideoDecoderMalfunctionErr` (the hardware decoder reset).
/// The session is rebuilt from the next keyframe's parameter sets, which are the same ones, so
/// a change of parameter sets cannot be the trigger.
#[must_use]
pub const fn session_lost(status: i32) -> bool {
    status == kVTInvalidSessionErr || status == kVTVideoDecoderMalfunctionErr
}

type Sink = Box<dyn Fn(DecodeOutcome) + Send + Sync>;

struct Shared {
    sink: Sink,
    /// Set by the output callback on a [`session_lost`] status; `decode` drops the session.
    lost: AtomicBool,
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

/// VPS, SPS and PPS of a 64×64 HEVC Main stream (Apple's encoder, `slopty-codec` round trip),
/// enough to build a format description and a session without a picture.
const WARM_UP_HEVC: [&[u8]; 3] = [
    &[
        0x40, 0x01, 0x0c, 0x03, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0xb0, 0x00, 0x00,
        0x03, 0x00, 0x00, 0x03, 0x00, 0x3c, 0x00, 0x00, 0x04, 0x30, 0x24,
    ],
    &[
        0x42, 0x01, 0x03, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0xb0, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x3c, 0x00, 0x00, 0xa0, 0x14, 0x20, 0x41, 0xc1, 0x8f, 0x88, 0x04, 0x3b, 0x91,
        0x48, 0x20, 0xb9, 0xf8, 0x4f, 0x42, 0xfa, 0x86, 0xf5, 0x43, 0xfa, 0xa8, 0x23, 0xd5, 0x50,
        0x4f, 0xaa, 0xa8, 0x2b, 0xd5, 0x55, 0x05, 0xfa, 0xaa, 0xa8, 0x33, 0xd5, 0x55, 0x50, 0x6f,
        0xaa, 0xaa, 0xa8, 0x3b, 0xd5, 0x55, 0x55, 0x07, 0xfa, 0xaa, 0xaa, 0xa8, 0x10, 0xf5, 0x55,
        0x55, 0x54, 0xa6, 0xe0, 0x40, 0x40, 0x40, 0x7f, 0x08, 0x04, 0x10,
    ],
    &[0x44, 0x01, 0xc0, 0x72, 0xf0, 0x5b, 0x24],
];

/// Create and drop one HEVC decompression session so the first real stream does not pay
/// for VideoToolbox's start-up in this process. Returns how long that took.
///
/// Measured 2026-09-05 (MEASUREMENTS.md, "start-up on a cold connection"): the first
/// `VTDecompressionSessionCreate` in a process takes ~150 ms, every later one ~3 ms, and the
/// stream worker that creates it holds every datagram behind it for that long — which the
/// reassembler then counted as a 150 ms link stall. Call this once, off the stream's path,
/// when a worker link comes up.
pub fn warm_up() -> Result<std::time::Duration, CodecError> {
    let started = std::time::Instant::now();
    let mut decoder = Decoder::new(VideoCodec::Hevc, |_frame| {});
    let sets: Vec<Vec<u8>> = WARM_UP_HEVC.iter().map(|nal| nal.to_vec()).collect();
    decoder.configure(&sets)?;
    drop(decoder);
    Ok(started.elapsed())
}

impl Decoder {
    /// A decoder that delivers pictures to `sink` on VideoToolbox's thread and drops failures.
    pub fn new(codec: VideoCodec, sink: impl Fn(DecodedFrame) + Send + Sync + 'static) -> Self {
        Self::with_outcomes(codec, move |outcome| {
            if let Ok(frame) = outcome {
                sink(frame);
            }
        })
    }

    /// A decoder that hands `sink` every submitted frame's outcome, on VideoToolbox's thread: the
    /// picture, or why there is none. A receiver needs the failures to ask for a refresh, and the
    /// successes to know which long-term references it really holds.
    pub fn with_outcomes(
        codec: VideoCodec,
        sink: impl Fn(DecodeOutcome) + Send + Sync + 'static,
    ) -> Self {
        Self {
            codec,
            session: None,
            format: None,
            parameter_sets: Vec::new(),
            shared: Arc::new(Shared { sink: Box::new(sink), lost: AtomicBool::new(false) }),
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
    ///
    /// One scan finds the parameter sets and the picture's units; the units are written once,
    /// length-prefixed, straight into the sample's block buffer.
    ///
    /// A lost session (see [`session_lost`]) is dropped with its parameter sets. A keyframe that
    /// finds it lost rebuilds it and is submitted again, so the frame that could restart the
    /// stream is not the one spent finding out; anything else fails until a keyframe comes, and
    /// [`Self::ready`] turns false to say so.
    pub fn decode(&mut self, annexb: &[u8], pts_us: u64) -> Result<(), CodecError> {
        if self.shared.lost.load(Ordering::Acquire) {
            tracing::info!("decoder session lost; waiting for a keyframe");
            self.reset();
        }
        let is_ps: fn(&[u8]) -> bool = match self.codec {
            VideoCodec::Hevc => hevc::is_parameter_set,
            VideoCodec::H264 => h264::is_parameter_set,
        };
        let unit = AccessUnit::parse(annexb, is_ps);
        let sets = unit.parameter_sets();
        if !sets.is_empty()
            && !sets.iter().copied().eq(self.parameter_sets.iter().map(Vec::as_slice))
        {
            self.rebuild(sets)?;
        }
        if self.session.is_none() {
            return Err(CodecError::NoParameterSets);
        }
        if unit.length_prefixed_len() == 0 {
            return Ok(());
        }
        let mut status = self.submit(&unit, pts_us)?;
        if session_lost(status) {
            self.reset();
            if !sets.is_empty() {
                tracing::info!(status, "decoder session lost; rebuilt from the keyframe");
                self.rebuild(sets)?;
                status = self.submit(&unit, pts_us)?;
                if session_lost(status) {
                    self.reset();
                }
            }
        }
        check("VTDecompressionSessionDecodeFrame", status)?;
        self.frames_in = self.frames_in.saturating_add(1);
        Ok(())
    }

    /// Configure for `sets` and remember them.
    fn rebuild(&mut self, sets: &[&[u8]]) -> Result<(), CodecError> {
        let owned: Vec<Vec<u8>> = sets.iter().map(|set| set.to_vec()).collect();
        let t0 = std::time::Instant::now();
        self.configure(&owned)?;
        tracing::debug!(ms = t0.elapsed().as_millis(), "decoder session configured");
        self.parameter_sets = owned;
        Ok(())
    }

    /// Hand one picture to the session; VideoToolbox's status for the submission.
    fn submit(&self, unit: &AccessUnit<'_>, pts_us: u64) -> Result<i32, CodecError> {
        let (Some(session), Some(format)) = (self.session.as_ref(), self.format.as_ref()) else {
            return Err(CodecError::NoParameterSets);
        };
        let sample = sample_buffer(unit, format, cf::time_us(pts_us))?;
        let mut info = VTDecodeInfoFlags::empty();
        // SAFETY: the sample buffer is valid and owned by us; the session outlives the call.
        // Frames are returned through the output callback, so no source refcon is needed.
        Ok(unsafe {
            session.decode_frame(
                &sample,
                VTDecodeFrameFlags::Frame_EnableAsynchronousDecompression,
                ptr::null_mut(),
                &raw mut info,
            )
        })
    }

    /// Forget the session and the parameter sets it was built from, so the next keyframe
    /// builds a new one even though its parameter sets are the same.
    fn reset(&mut self) {
        if let Some(session) = self.session.take() {
            // SAFETY: invalidating a session is allowed in any state, a dead one included; once
            // it returns the old session calls back no more, so its verdict can be cleared.
            unsafe { session.invalidate() }
        }
        self.shared.lost.store(false, Ordering::Release);
        self.format = None;
        self.parameter_sets.clear();
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
        // Full-range bi-planar 4:2:0, the format the worker captures and encodes, so the decoder
        // writes its output directly and runs no conversion pass; GPUI's surface path samples
        // the two planes through `CVMetalTextureCache`.
        let format_type = CFNumber::new_i32(i32::from_ne_bytes(
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange.to_ne_bytes(),
        ));
        let attrs = CFDictionary::<CFString, CFType>::from_slices(
            &[
                // SAFETY: framework-provided constant string.
                unsafe { kCVPixelBufferIOSurfacePropertiesKey },
                // SAFETY: framework-provided constant string.
                unsafe { kCVPixelBufferMetalCompatibilityKey },
                // SAFETY: framework-provided constant string.
                unsafe { kCVPixelBufferPixelFormatTypeKey },
            ],
            &[&CFDictionary::<CFString, CFType>::empty(), cf::boolean(true), &format_type],
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
    // SAFETY: the refcon was created from `Arc::as_ptr` on the decoder's `Shared`, which the
    // `Decoder` keeps alive until the session is invalidated (see `Drop`).
    let shared: &Shared = unsafe { &*refcon.cast::<Shared>() };
    let pts_us = cf::micros(pts).unwrap_or(0);
    let image = NonNull::new(image);
    let (true, false, Some(image)) =
        (status == 0, flags.contains(VTDecodeInfoFlags::FrameDropped), image)
    else {
        tracing::debug!(status, ?flags, "decoder returned no picture");
        if session_lost(status) {
            shared.lost.store(true, Ordering::Release);
        }
        (shared.sink)(Err(DecodeFailure { pts_us, status }));
        return;
    };
    // SAFETY: VideoToolbox hands the callback a borrowed image buffer; retaining it here gives
    // the sink its own reference.
    let image = unsafe { CFRetained::retain(image) };
    (shared.sink)(Ok(DecodedFrame { image: PixelBuffer(image), pts_us }));
}

pub fn format_description(
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
        VideoCodec::Hevc => (
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

/// A sample buffer holding `unit`'s picture as length-prefixed NAL units, written straight
/// into the block buffer CoreMedia allocates.
fn sample_buffer(
    unit: &AccessUnit<'_>,
    format: &CMFormatDescription,
    pts: CMTime,
) -> Result<CFRetained<CMSampleBuffer>, CodecError> {
    let size = unit.length_prefixed_len();
    let mut block: *mut CMBlockBuffer = ptr::null_mut();
    // SAFETY: a NULL memory block asks CoreMedia to allocate `size` bytes itself, now
    // (`kCMBlockBufferAssureMemoryNowFlag`); the out-pointer is valid.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            ptr::null_mut(),
            size,
            None,
            ptr::null(),
            0,
            size,
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
    let mut contiguous: usize = 0;
    let mut data: *mut c_char = ptr::null_mut();
    // SAFETY: CoreMedia rule: the out-pointers are valid; the pointer returned addresses
    // `contiguous` bytes owned by the block buffer, which lives past the write below.
    let status =
        unsafe { block.data_pointer(0, &raw mut contiguous, ptr::null_mut(), &raw mut data) };
    check("CMBlockBufferGetDataPointer", status)?;
    if data.is_null() || contiguous < size {
        return Err(CodecError::Os { call: "CMBlockBufferGetDataPointer", status: -1 });
    }
    // SAFETY: `data` addresses `contiguous >= size` writable bytes of the block buffer's one
    // allocation, which nothing else references yet.
    let out = unsafe { std::slice::from_raw_parts_mut(data.cast::<u8>(), size) };
    unit.write_length_prefixed(out)?;
    let timing = CMSampleTimingInfo {
        // SAFETY: framework-provided constant.
        duration: unsafe { kCMTimeInvalid },
        presentationTimeStamp: pts,
        // SAFETY: framework-provided constant.
        decodeTimeStamp: unsafe { kCMTimeInvalid },
    };
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

#[cfg(test)]
impl Decoder {
    /// Invalidate the session but keep it, the state the system leaves behind when it takes a
    /// session away: every later call on it answers `kVTInvalidSessionErr`.
    fn kill_session(&self) {
        if let Some(session) = self.session.as_ref() {
            // SAFETY: invalidating a live session is always allowed; it stays retained here.
            unsafe { session.invalidate() }
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
#[expect(clippy::arithmetic_side_effects, reason = "test timestamps on a handful of frames")]
mod recovery_tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use objc2_core_video::CVPixelBufferCreate;

    use super::*;
    use crate::{EncodedPacket, Encoder, EncoderConfig, FrameOptions};

    /// A picture for the encoder; what it shows does not matter here.
    fn picture() -> CFRetained<CVPixelBuffer> {
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: CoreVideo rule: a valid out-pointer and no attributes.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                320,
                180,
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                None,
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, 0, "CVPixelBufferCreate");
        // SAFETY: +1 reference from the create call.
        unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) }
    }

    /// Encoded access units, a keyframe wherever `keyframes` says.
    fn stream(keyframes: &[bool]) -> Vec<EncodedPacket> {
        let (tx, rx) = mpsc::channel();
        let config = EncoderConfig {
            width: 320,
            height: 180,
            codec: VideoCodec::Hevc,
            fps: 60,
            bitrate_bps: 2_000_000,
        };
        let encoder = Encoder::new(config, move |packet| {
            let _receiver_gone = tx.send(packet);
        })
        .unwrap();
        let image = picture();
        for (i, &keyframe) in keyframes.iter().enumerate() {
            let options = FrameOptions { force_keyframe: keyframe, ..FrameOptions::default() };
            encoder.encode(&image, u64::try_from(i).unwrap() * 16_667, &options).unwrap();
        }
        encoder.flush().unwrap();
        let packets: Vec<EncodedPacket> = (0..keyframes.len())
            .map_while(|_| rx.recv_timeout(Duration::from_secs(30)).ok())
            .collect();
        assert_eq!(packets.len(), keyframes.len(), "every picture encoded");
        let got: Vec<bool> = packets.iter().map(|p| p.keyframe).collect();
        assert_eq!(got, keyframes, "keyframes where asked");
        packets
    }

    fn decoder() -> (Decoder, mpsc::Receiver<Result<u64, DecodeFailure>>) {
        let (tx, rx) = mpsc::channel();
        let decoder = Decoder::with_outcomes(VideoCodec::Hevc, move |outcome| {
            let _receiver_gone = tx.send(outcome.map(|frame| frame.pts_us));
        });
        (decoder, rx)
    }

    /// The next outcome. The wait is long because VideoToolbox runs far slower from some
    /// volumes, not because an outcome is expected to take it.
    #[track_caller]
    fn next(rx: &mpsc::Receiver<Result<u64, DecodeFailure>>) -> Result<u64, DecodeFailure> {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(outcome) => outcome,
            Err(e) => panic!("no outcome: {e}"),
        }
    }

    /// Wait for everything submitted to come back.
    fn settle(decoder: &Decoder) {
        if let Some(session) = decoder.session.as_ref() {
            // SAFETY: a live session; the call only blocks until pending frames are emitted.
            let status = unsafe { session.wait_for_asynchronous_frames() };
            assert_eq!(status, 0, "VTDecompressionSessionWaitForAsynchronousFrames");
        }
    }

    /// A frame the decoder could not use comes back as a failure, not as silence: a P-frame
    /// whose reference it never decoded, and a lost session reported by the callback. The latter
    /// drops the session, so a P-frame is refused until a keyframe builds a new one.
    #[test]
    fn a_frame_without_a_picture_comes_back_as_a_failure() {
        let packets = stream(&[true, false, true]);
        let (mut decoder, rx) = decoder();
        // The keyframe's parameter sets alone build the session without a picture, so the P-frame
        // after it names a reference the decoder never saw: the state a new session is in when
        // anything but a keyframe reaches it.
        let mut sets_only = Vec::new();
        for set in AccessUnit::parse(&packets[0].data, hevc::is_parameter_set).parameter_sets() {
            sets_only.extend_from_slice(&crate::annexb::START_CODE);
            sets_only.extend_from_slice(set);
        }
        decoder.decode(&sets_only, 0).unwrap();
        decoder.decode(&packets[1].data, 1).unwrap();
        settle(&decoder);
        let failed = next(&rx).unwrap_err();
        assert_eq!(failed.pts_us, 1);
        assert_ne!(failed.status, 0);
        assert!(!failed.session_lost(), "the frame's fault, not the session's: {failed:?}");
        assert!(decoder.ready());

        // SAFETY: the refcon is the decoder's own `Shared`, alive for the call; a null image
        // with an error status is what VideoToolbox passes for a failed frame.
        unsafe {
            output_callback(
                Arc::as_ptr(&decoder.shared).cast_mut().cast::<c_void>(),
                ptr::null_mut(),
                kVTInvalidSessionErr,
                VTDecodeInfoFlags::empty(),
                ptr::null_mut(),
                cf::time_us(2),
                cf::time_us(0),
            );
        }
        let lost = next(&rx).unwrap_err();
        assert_eq!((lost.pts_us, lost.session_lost()), (2, true));
        assert!(matches!(decoder.decode(&packets[1].data, 3), Err(CodecError::NoParameterSets)));
        assert!(!decoder.ready(), "the session went with the verdict");
        decoder.decode(&packets[2].data, 4).unwrap();
        settle(&decoder);
        assert_eq!(next(&rx), Ok(4), "the next keyframe decodes on a new session");
    }

    /// A session the system took away is dropped with its parameter sets, which the next
    /// keyframe carries unchanged: a P-frame that finds it dead fails and leaves the decoder
    /// waiting, and a keyframe that finds it dead rebuilds it and is decoded after all.
    #[test]
    fn a_lost_session_is_rebuilt_from_the_next_keyframe() {
        let packets = stream(&[true, false, false, true, false, true]);
        let (mut decoder, rx) = decoder();
        for (pts, packet) in (0..).zip(&packets[..2]) {
            decoder.decode(&packet.data, pts).unwrap();
        }
        settle(&decoder);
        assert_eq!((next(&rx), next(&rx)), (Ok(0), Ok(1)));

        decoder.kill_session();
        let refused = decoder.decode(&packets[2].data, 2);
        assert!(
            matches!(refused, Err(CodecError::Os { status, .. }) if session_lost(status)),
            "{refused:?}"
        );
        assert!(!decoder.ready());
        decoder.decode(&packets[3].data, 3).unwrap();
        decoder.decode(&packets[4].data, 4).unwrap();
        settle(&decoder);
        assert_eq!((next(&rx), next(&rx)), (Ok(3), Ok(4)));

        decoder.kill_session();
        decoder.decode(&packets[5].data, 5).unwrap();
        settle(&decoder);
        assert_eq!(next(&rx), Ok(5), "the keyframe that found it dead was not wasted");
        assert!(rx.try_recv().is_err(), "nothing else came back");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 MB Annex B keyframe: the warm-up parameter sets, then four slices of noise-free
    /// bytes with no start-code lookalikes.
    fn keyframe() -> Vec<u8> {
        let mut out = Vec::new();
        for set in WARM_UP_HEVC {
            out.extend_from_slice(&crate::annexb::START_CODE);
            out.extend_from_slice(set);
        }
        for n in 0..4_usize {
            out.extend_from_slice(&crate::annexb::START_CODE);
            out.push(0x26);
            out.extend(
                (0..250_000_usize)
                    .map(|i| u8::try_from(i.wrapping_add(n) % 250).unwrap_or(0).wrapping_add(1)),
            );
        }
        out
    }

    /// What turning one received access unit into a sample buffer costs before VideoToolbox
    /// sees it. `docs/MEASUREMENTS.md` records runs.
    #[test]
    #[ignore = "a measurement; run with --run-ignored only --no-capture in release"]
    fn access_unit_conversion_cost() {
        let unit = keyframe();
        let sets: Vec<Vec<u8>> = WARM_UP_HEVC.iter().map(|s| s.to_vec()).collect();
        let format = format_description(VideoCodec::Hevc, &sets).unwrap();
        let rounds = 500_u32;
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            let access = AccessUnit::parse(&unit, hevc::is_parameter_set);
            assert_eq!(access.parameter_sets().len(), 3);
            let sample = sample_buffer(&access, &format, cf::time_us(0)).unwrap();
            drop(sample);
        }
        let per = started.elapsed() / rounds;
        eprintln!("keyframe {} B: {per:?} per access unit", unit.len());
    }
}

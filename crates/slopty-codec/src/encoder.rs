//! `VTCompressionSession` for interactive streaming (macOS worker only).

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, Type as _,
};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMSampleAttachmentKey_NotSync,
    kCMTimeInvalid, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC,
};
use objc2_core_video::{CVPixelBuffer, kCVImageBufferYCbCrMatrix_ITU_R_709_2};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, kVTCompressionPropertyKey_AllowFrameReordering,
    kVTCompressionPropertyKey_AllowOpenGOP, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_DataRateLimits, kVTCompressionPropertyKey_EnableLTR,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxFrameDelayCount,
    kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaximumRealTimeFrameRate,
    kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTCompressionPropertyKey_YCbCrMatrix, kVTEncodeFrameOptionKey_AcknowledgedLTRTokens,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTEncodeFrameOptionKey_ForceLTRRefresh,
    kVTProfileLevel_H264_High_AutoLevel, kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTPropertyNotSupportedErr, kVTSampleAttachmentKey_RequireLTRAcknowledgementToken,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};
use slopty_proto::screen::VideoCodec;

use crate::cf::{self, check};
use crate::video::{EncodedPacket, EncoderConfig, FrameOptions, VideoEncoder};
use crate::{CodecError, PixelBuffer, annexb};

/// Which rate-control mode a session runs in. The worker only ever runs the low-latency one;
/// the other exists for the measurement that rejected it (`experiments` feature).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RateControl {
    /// `EnableLowLatencyRateControl` in the encoder specification (infinite GOP, no
    /// reordering, LTR available) with `AverageBitRate` + `DataRateLimits`: the ruled mode.
    #[default]
    LowLatency,
    /// macOS 26's `VariableBitRate` + `VBVMaxBitRate` + `VBVBufferDuration` on a session
    /// *without* low-latency rate control (the header says they are incompatible with it).
    /// Measured against `LowLatency` in MEASUREMENTS.md; not used by the worker.
    #[cfg(feature = "experiments")]
    Vbv,
}

type Sink = Box<dyn Fn(EncodedPacket) + Send + Sync>;

fn retain(value: &CFType) -> CFRetained<CFType> {
    CFType::retain(value)
}

/// The per-frame refcon of a frame submitted as an LTR refresh. VideoToolbox hands a frame's
/// `sourceFrameRefcon` back to the output callback with that frame and never dereferences it,
/// so the flag rides with its own frame whatever is in flight or dropped around it.
const REFRESH_REFCON: usize = 1;

/// The `sourceFrameRefcon` for a frame: [`REFRESH_REFCON`] for a refresh, null otherwise.
const fn frame_refcon(refresh: bool) -> *mut c_void {
    if refresh { ptr::without_provenance_mut(REFRESH_REFCON) } else { ptr::null_mut() }
}

/// Whether the frame the callback got was submitted as a refresh.
fn is_refresh(refcon: *mut c_void) -> bool {
    refcon.addr() == REFRESH_REFCON
}

struct Shared {
    sink: Sink,
    codec: VideoCodec,
}

/// A hardware encoder.
pub struct Encoder {
    session: CFRetained<VTCompressionSession>,
    config: EncoderConfig,
    rate_control: RateControl,
    ltr: bool,
    // Declared last so it outlives the session's `Drop` (the callback's refcon points at it).
    _shared: Arc<Shared>,
}

// SAFETY: VideoToolbox sessions are documented as usable from any thread:
// `VTSessionSetProperty`, `VTCompressionSessionEncodeFrame` and `VTCompressionSessionInvalidate`
// are serialised inside the framework, and the output callback already arrives on a
// VideoToolbox thread. `Shared` is only read, through the `Send + Sync` sink.
#[expect(clippy::non_send_fields_in_send_ty, reason = "VTCompressionSession is thread-safe")]
unsafe impl Send for Encoder {}
// SAFETY: as above; every `&self` method is a thread-safe VideoToolbox call.
unsafe impl Sync for Encoder {}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder")
            .field("config", &self.config)
            .field("rate_control", &self.rate_control)
            .field("ltr", &self.ltr)
            .finish_non_exhaustive()
    }
}

impl Encoder {
    /// Create and configure a low-latency session. Packets are delivered to `sink` on
    /// VideoToolbox's thread.
    pub fn new(
        config: EncoderConfig,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        Self::with_rate_control(config, RateControl::LowLatency, Box::new(sink))
    }

    /// A session in another rate-control mode, for the measurement that compares them.
    #[cfg(feature = "experiments")]
    pub fn experiment(
        config: EncoderConfig,
        rate_control: RateControl,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        Self::with_rate_control(config, rate_control, Box::new(sink))
    }

    fn with_rate_control(
        config: EncoderConfig,
        rate_control: RateControl,
        sink: Sink,
    ) -> Result<Self, CodecError> {
        let shared = Arc::new(Shared { sink, codec: config.codec });
        // SAFETY: framework-provided constant string.
        let hardware_key =
            unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder };
        let spec = match rate_control {
            RateControl::LowLatency => {
                // SAFETY: framework-provided constant string.
                let low_latency_key =
                    unsafe { kVTVideoEncoderSpecification_EnableLowLatencyRateControl };
                CFDictionary::<CFString, CFType>::from_slices(
                    &[low_latency_key, hardware_key],
                    &[cf::boolean(true), cf::boolean(true)],
                )
            }
            #[cfg(feature = "experiments")]
            RateControl::Vbv => {
                CFDictionary::<CFString, CFType>::from_slices(&[hardware_key], &[cf::boolean(true)])
            }
        };
        let codec_type = match config.codec {
            VideoCodec::Hevc => kCMVideoCodecType_HEVC,
            VideoCodec::H264 => kCMVideoCodecType_H264,
        };
        let mut raw: *mut VTCompressionSession = ptr::null_mut();
        // SAFETY: all pointers are valid for the call; the refcon is the `Arc<Shared>` kept
        // alive by this `Encoder` until the session is invalidated (see `Drop`).
        let status = unsafe {
            VTCompressionSession::create(
                None,
                i32::try_from(config.width).unwrap_or(i32::MAX),
                i32::try_from(config.height).unwrap_or(i32::MAX),
                codec_type,
                Some(spec.as_opaque()),
                None,
                None,
                Some(output_callback),
                Arc::as_ptr(&shared).cast_mut().cast::<c_void>(),
                NonNull::from(&mut raw),
            )
        };
        check("VTCompressionSessionCreate", status)?;
        let Some(raw) = NonNull::new(raw) else {
            return Err(CodecError::Os { call: "VTCompressionSessionCreate", status: -1 });
        };
        // SAFETY: `create` returned a +1 reference.
        let session = unsafe { CFRetained::from_raw(raw) };
        let mut encoder = Self { session, config, rate_control, ltr: false, _shared: shared };
        encoder.configure()?;
        // SAFETY: the session is fully configured; this only pre-allocates encoder resources.
        let status = unsafe { encoder.session.prepare_to_encode_frames() };
        check("VTCompressionSessionPrepareToEncodeFrames", status)?;
        Ok(encoder)
    }

    /// The settings in force.
    #[must_use]
    pub const fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// True when the encoder accepted long-term references.
    #[must_use]
    pub const fn ltr_enabled(&self) -> bool {
        self.ltr
    }

    fn set(&self, key: &CFString, value: &CFType, call: &'static str) -> Result<(), CodecError> {
        cf::set_property(&self.session, key, value, call)
    }

    /// Set one public `kVTCompressionPropertyKey_*` on the live session; `call` names it in
    /// the error. The worker's property set lives in `configure`.
    #[cfg(feature = "experiments")]
    pub fn set_property(
        &self,
        key: &CFString,
        value: &CFType,
        call: &'static str,
    ) -> Result<(), CodecError> {
        self.set(key, value, call)
    }

    /// Set a property that some encoders do not implement; unsupported is logged, not fatal.
    fn set_optional(&self, key: &CFString, value: &CFType, call: &'static str) -> bool {
        match self.set(key, value, call) {
            Ok(()) => true,
            Err(CodecError::Os { status, .. }) if status == kVTPropertyNotSupportedErr => {
                tracing::debug!(
                    call,
                    "encoder does not support this property (expected on Apple silicon for AllowOpenGOP, MaxFrameDelayCount, PrioritizeEncodingSpeedOverQuality)"
                );
                false
            }
            Err(err) => {
                tracing::warn!(call, %err, "setting encoder property failed");
                false
            }
        }
    }

    fn configure(&mut self) -> Result<(), CodecError> {
        let fps = f64::from(self.config.fps);
        // `(key, value, name, required)`. Optional ones are absent on some encoders.
        let table: [(&CFString, CFRetained<CFType>, &'static str, bool); 9] = [
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_RealTime },
                retain(cf::boolean(true)),
                "RealTime",
                true,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_AllowFrameReordering },
                retain(cf::boolean(false)),
                "AllowFrameReordering",
                true,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_AllowOpenGOP },
                retain(cf::boolean(false)),
                "AllowOpenGOP",
                false,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_MaxFrameDelayCount },
                retain(&cf::int(0)),
                "MaxFrameDelayCount",
                false,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality },
                retain(cf::boolean(true)),
                "PrioritizeEncodingSpeedOverQuality",
                false,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_ExpectedFrameRate },
                retain(&cf::float(fps)),
                "ExpectedFrameRate",
                true,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_MaximumRealTimeFrameRate },
                retain(&cf::int(120)),
                "MaximumRealTimeFrameRate",
                false,
            ),
            (
                // Infinite GOP: keyframes only on request.
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_MaxKeyFrameInterval },
                retain(&cf::int(i64::from(i32::MAX))),
                "MaxKeyFrameInterval",
                true,
            ),
            (
                // The capture asks ScreenCaptureKit for BT.709 (`slopty_capture`), and the stream
                // says so in its VUI: the client's shader reads the matrix back off the decoded
                // buffer instead of assuming one.
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_YCbCrMatrix },
                // SAFETY: framework-provided constant string.
                retain(unsafe { kCVImageBufferYCbCrMatrix_ITU_R_709_2 }),
                "YCbCrMatrix",
                true,
            ),
        ];
        for (key, value, name, required) in &table {
            if *required {
                self.set(key, value, name)?;
            } else {
                self.set_optional(key, value, name);
            }
        }
        let profile: &CFString = match self.config.codec {
            // SAFETY: framework-provided constant string.
            VideoCodec::Hevc => unsafe { kVTProfileLevel_HEVC_Main_AutoLevel },
            // SAFETY: framework-provided constant string.
            VideoCodec::H264 => unsafe { kVTProfileLevel_H264_High_AutoLevel },
        };
        // SAFETY: framework-provided constant string.
        let profile_key = unsafe { kVTCompressionPropertyKey_ProfileLevel };
        self.set(profile_key, profile, "ProfileLevel")?;
        self.set_bitrate(self.config.bitrate_bps)?;
        // SAFETY: framework-provided constant string.
        let ltr_key = unsafe { kVTCompressionPropertyKey_EnableLTR };
        self.ltr = self.set_optional(ltr_key, cf::boolean(true), "EnableLTR");
        Ok(())
    }

    /// Tell the session how many frames a second it is now being given.
    ///
    /// `AverageBitRate` is spent over a second however many frames arrive, so the cadence change
    /// alone already gives the surviving frames the skipped ones' bytes. `ExpectedFrameRate` is
    /// the hint the rate controller sizes its first frames from; it would still describe the
    /// old cadence otherwise.
    pub fn set_frame_rate(&self, fps: u16) -> Result<(), CodecError> {
        let fps = f64::from(fps.max(1));
        // SAFETY: framework-provided constant string.
        let expected_key = unsafe { kVTCompressionPropertyKey_ExpectedFrameRate };
        self.set(expected_key, &cf::float(fps), "ExpectedFrameRate")?;
        #[cfg(feature = "experiments")]
        if self.rate_control == RateControl::Vbv {
            // SAFETY: framework-provided constant string.
            let duration_key =
                unsafe { objc2_video_toolbox::kVTCompressionPropertyKey_VBVBufferDuration };
            self.set(duration_key, &cf::float(2.0 / fps), "VBVBufferDuration")?;
        }
        Ok(())
    }

    /// Change the target bitrate on the fly (bits per second).
    pub fn set_bitrate(&self, bps: u32) -> Result<(), CodecError> {
        match self.rate_control {
            RateControl::LowLatency => {
                // SAFETY: framework-provided constant string.
                let average_key = unsafe { kVTCompressionPropertyKey_AverageBitRate };
                self.set(average_key, &cf::int(i64::from(bps)), "AverageBitRate")?;
                // Hard cap per second: 1.25× the average, in bytes.
                let bytes_per_second = i64::from(bps).saturating_mul(5) / 32;
                let limits = CFArray::<CFNumber>::from_retained_objects(&[
                    cf::int(bytes_per_second),
                    cf::float(1.0),
                ]);
                // SAFETY: framework-provided constant string.
                let limits_key = unsafe { kVTCompressionPropertyKey_DataRateLimits };
                self.set_optional(limits_key, limits.as_opaque(), "DataRateLimits");
            }
            #[cfg(feature = "experiments")]
            RateControl::Vbv => experiments::set_vbv_bitrate(self, bps)?,
        }
        Ok(())
    }

    /// Submit one picture. `pts_us` is echoed on the packet; use the capture timestamp.
    pub fn encode(
        &self,
        image: &CVPixelBuffer,
        pts_us: u64,
        options: &FrameOptions,
    ) -> Result<(), CodecError> {
        let mut keys: Vec<&CFString> = Vec::new();
        let mut values: Vec<&CFType> = Vec::new();
        let tokens: CFRetained<CFArray<CFNumber>>;
        let has_acks = self.ltr && !options.acked_ltr.is_empty();
        if options.force_keyframe {
            // SAFETY: framework-provided constant string.
            let key = unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame };
            keys.push(key);
            values.push(CFBoolean::new(true));
        }
        let refresh = options.force_ltr_refresh && self.ltr;
        if refresh {
            // SAFETY: framework-provided constant string.
            let key = unsafe { kVTEncodeFrameOptionKey_ForceLTRRefresh };
            keys.push(key);
            values.push(CFBoolean::new(true));
        }
        if has_acks {
            let numbers: Vec<CFRetained<CFNumber>> = options
                .acked_ltr
                .iter()
                .map(|&t| cf::int(i64::try_from(t).unwrap_or(i64::MAX)))
                .collect();
            tokens = CFArray::from_retained_objects(&numbers);
            // SAFETY: framework-provided constant string.
            let key = unsafe { kVTEncodeFrameOptionKey_AcknowledgedLTRTokens };
            keys.push(key);
            values.push(&tokens);
        }
        let properties = (!keys.is_empty())
            .then(|| CFDictionary::<CFString, CFType>::from_slices(&keys, &values));
        let mut flags = VTEncodeInfoFlags::empty();
        // SAFETY: the image buffer is valid and retained by the caller for the call; the
        // session retains it as long as the encoder needs it. The source refcon is a tag the
        // framework only hands back to the callback (`frame_refcon`), never a pointer it reads.
        let status = unsafe {
            self.session.encode_frame(
                image,
                cf::time_us(pts_us),
                kCMTimeInvalid,
                properties.as_deref().map(CFDictionary::as_opaque),
                frame_refcon(refresh),
                &raw mut flags,
            )
        };
        check("VTCompressionSessionEncodeFrame", status)
    }

    /// Wait for every submitted frame to come out of the sink.
    pub fn flush(&self) -> Result<(), CodecError> {
        // SAFETY: an invalid time means "everything submitted so far".
        let status = unsafe { self.session.complete_frames(kCMTimeInvalid) };
        check("VTCompressionSessionCompleteFrames", status)
    }
}

/// [`Encoder`] as the worker's [`VideoEncoder`]: it takes the frames ScreenCaptureKit hands
/// over, which are `IOSurface`-backed pixel buffers.
#[derive(Debug)]
pub struct VideoToolbox(Encoder);

impl VideoEncoder for VideoToolbox {
    type Image = PixelBuffer;

    fn new(
        config: EncoderConfig,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        Encoder::new(config, sink).map(Self)
    }

    fn encode(
        &self,
        image: &PixelBuffer,
        pts_us: u64,
        options: &FrameOptions,
    ) -> Result<(), CodecError> {
        self.0.encode(image.as_cv(), pts_us, options)
    }

    fn set_bitrate(&self, bps: u32) -> Result<(), CodecError> {
        self.0.set_bitrate(bps)
    }

    fn set_frame_rate(&self, fps: u16) -> Result<(), CodecError> {
        self.0.set_frame_rate(fps)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: invalidation stops callbacks; afterwards the session never touches the
        // refcon again, so the `Arc<Shared>` may be released with `self`.
        unsafe { self.session.invalidate() }
    }
}

unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    source: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let refresh = is_refresh(source);
    if status != 0 || flags.contains(VTEncodeInfoFlags::FrameDropped) {
        // A refresh lost here is asked for again: the receiver repeats its request until a
        // picture it can decode arrives.
        tracing::debug!(status, ?flags, refresh, "encoder dropped a frame");
        return;
    }
    // SAFETY: the refcon was created from `Arc::as_ptr` on the encoder's `Shared`, which the
    // `Encoder` keeps alive until the session is invalidated (see `Drop`).
    let shared: &Shared = unsafe { &*refcon.cast::<Shared>() };
    let Some(sample) = NonNull::new(sample) else { return };
    // SAFETY: the sample buffer is valid for the duration of the callback.
    let sample: &CMSampleBuffer = unsafe { sample.as_ref() };
    match packet(sample, shared.codec, refresh) {
        Ok(packet) => (shared.sink)(packet),
        Err(e) => tracing::warn!(error = %e, "encoder output is not an access unit; dropped"),
    }
}

/// VideoToolbox's output as a packet: the parameter sets in front of a keyframe, then the
/// sample's bytes copied once and their length prefixes rewritten to start codes in place.
fn packet(
    sample: &CMSampleBuffer,
    codec: VideoCodec,
    refresh: bool,
) -> Result<EncodedPacket, CodecError> {
    // SAFETY: valid sample buffer.
    let block = unsafe { sample.data_buffer() }
        .ok_or(CodecError::Os { call: "CMSampleBufferGetDataBuffer", status: -1 })?;
    let (keyframe, ltr_token) = attachments(sample);
    // SAFETY: valid sample buffer.
    let pts_us = cf::micros(unsafe { sample.presentation_time_stamp() }).unwrap_or(0);
    let format = if keyframe {
        // SAFETY: valid sample buffer.
        unsafe { sample.format_description() }
    } else {
        None
    };
    let sets = match &format {
        Some(format) => parameter_sets(format, codec)?,
        None => Vec::new(),
    };
    // SAFETY: valid block buffer.
    let body = unsafe { block.data_length() };
    let head =
        sets.iter().fold(0_usize, |sum, set| sum.saturating_add(4).saturating_add(set.len()));
    let mut data = Vec::with_capacity(head.saturating_add(body));
    annexb::prepend_parameter_sets(&mut data, sets.iter().copied());
    if body > 0
        && let Some(spare) = NonNull::new(data.spare_capacity_mut().as_mut_ptr())
    {
        // SAFETY: CoreMedia rule: `CMBlockBufferCopyDataBytes` writes exactly `body` bytes to the
        // destination, which is the vector's spare capacity (at least `body` bytes, reserved
        // above).
        let status = unsafe { block.copy_data_bytes(0, body, spare.cast::<c_void>()) };
        check("CMBlockBufferCopyDataBytes", status)?;
        // SAFETY: the copy succeeded, so the `body` bytes past `head` are initialised.
        unsafe {
            data.set_len(head.saturating_add(body));
        }
    }
    let units = data.get_mut(head..).unwrap_or_default();
    annexb::length_prefixed_to_annexb_in_place(units)?;
    Ok(EncodedPacket { data, keyframe, ltr_token, ltr_refresh: refresh, pts_us })
}

/// `(is_keyframe, ltr_token)` from the sample's first attachment dictionary.
fn attachments(sample: &CMSampleBuffer) -> (bool, Option<u64>) {
    // SAFETY: valid sample buffer; `false` never allocates.
    let Some(array) = (unsafe { sample.sample_attachments_array(false) }) else {
        return (true, None);
    };
    // SAFETY: CoreMedia documents the array's elements as CFDictionaries keyed by CFString.
    let array: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(array) };
    let Some(dict) = array.get(0) else { return (true, None) };
    // SAFETY: framework-provided constant string.
    let not_sync_key = unsafe { kCMSampleAttachmentKey_NotSync };
    let not_sync = dict
        .get(not_sync_key)
        .and_then(|v| v.downcast::<CFBoolean>().ok())
        .is_some_and(|b| b.as_bool());
    // SAFETY: framework-provided constant string.
    let token_key = unsafe { kVTSampleAttachmentKey_RequireLTRAcknowledgementToken };
    let token = dict
        .get(token_key)
        .and_then(|v| v.downcast::<CFNumber>().ok())
        .and_then(|n| n.as_i64())
        .and_then(|n| u64::try_from(n).ok());
    (!not_sync, token)
}

/// The parameter sets of a format description, borrowed from it. The stream's NAL lengths must
/// be four bytes, the size of a start code, for the in-place rewrite; VideoToolbox's are.
fn parameter_sets(
    format: &CMFormatDescription,
    codec: VideoCodec,
) -> Result<Vec<&[u8]>, CodecError> {
    let mut sets = Vec::new();
    let mut count: usize = 0;
    let mut nal_length: std::ffi::c_int = 4;
    let mut index = 0_usize;
    loop {
        let mut ptr: *const u8 = ptr::null();
        let mut size: usize = 0;
        let status = match codec {
            // SAFETY: every out-pointer is valid; the parameter set bytes are owned by the
            // format description, which outlives the returned slices.
            VideoCodec::Hevc => unsafe {
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format,
                    index,
                    &raw mut ptr,
                    &raw mut size,
                    &raw mut count,
                    &raw mut nal_length,
                )
            },
            // SAFETY: as above.
            VideoCodec::H264 => unsafe {
                CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    format,
                    index,
                    &raw mut ptr,
                    &raw mut size,
                    &raw mut count,
                    &raw mut nal_length,
                )
            },
        };
        if status != 0 || ptr.is_null() || size == 0 {
            break;
        }
        // SAFETY: CoreMedia returned `size` readable bytes at `ptr`, owned by `format`.
        sets.push(unsafe { std::slice::from_raw_parts(ptr, size) });
        index = index.saturating_add(1);
        if index >= count {
            break;
        }
    }
    if nal_length != 4 {
        return Err(CodecError::MalformedNal { offset: 0 });
    }
    Ok(sets)
}

/// The rate-control comparison and the property probes behind the encoder's DECISIONS entries.
#[cfg(feature = "experiments")]
mod experiments {
    use std::ptr::{self, NonNull};

    use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_video_toolbox::{
        VTSession, VTSessionCopySupportedPropertyDictionary,
        kVTCompressionPropertyKey_MaxAllowedFrameQP, kVTCompressionPropertyKey_MinAllowedFrameQP,
        kVTCompressionPropertyKey_VBVBufferDuration, kVTCompressionPropertyKey_VBVMaxBitRate,
        kVTCompressionPropertyKey_VariableBitRate,
    };

    use super::Encoder;
    use crate::{CodecError, cf};

    pub(super) fn set_vbv_bitrate(encoder: &Encoder, bps: u32) -> Result<(), CodecError> {
        // SAFETY: framework-provided constant string.
        let vbr_key = unsafe { kVTCompressionPropertyKey_VariableBitRate };
        encoder.set(vbr_key, &cf::int(i64::from(bps)), "VariableBitRate")?;
        // The same 1.25× peak as the low-latency mode's data-rate limit.
        let peak = i64::from(bps).saturating_mul(5) / 4;
        // SAFETY: framework-provided constant string.
        let peak_key = unsafe { kVTCompressionPropertyKey_VBVMaxBitRate };
        encoder.set(peak_key, &cf::int(peak), "VBVMaxBitRate")?;
        // Two frames of buffer: the smallest model that still lets a frame differ from the
        // average (the default is 2.5 s, a delay this pipeline cannot pay).
        let duration = 2.0 / f64::from(encoder.config.fps.max(1));
        // SAFETY: framework-provided constant string.
        let duration_key = unsafe { kVTCompressionPropertyKey_VBVBufferDuration };
        encoder.set(duration_key, &cf::float(duration), "VBVBufferDuration")
    }

    impl Encoder {
        /// Try the optional quality keys on this session and report each `OSStatus` (0 =
        /// accepted, `kVTPropertyNotSupportedErr` = this encoder has no such knob). Leaves the
        /// accepted ones set, so call it on a throwaway session.
        #[must_use]
        pub fn probe_quality_keys(&self) -> Vec<(&'static str, i32)> {
            let keys: [(&CFString, CFRetained<CFNumber>, &'static str); 2] = [
                (
                    // SAFETY: framework-provided constant string.
                    unsafe { kVTCompressionPropertyKey_MaxAllowedFrameQP },
                    cf::int(45),
                    "MaxAllowedFrameQP",
                ),
                (
                    // SAFETY: framework-provided constant string.
                    unsafe { kVTCompressionPropertyKey_MinAllowedFrameQP },
                    cf::int(10),
                    "MinAllowedFrameQP",
                ),
            ];
            keys.iter()
                .map(|(key, value, name)| {
                    let status = match self.set(key, value, name) {
                        Ok(()) => 0,
                        Err(CodecError::Os { status, .. }) => status,
                        Err(_other) => -1,
                    };
                    (*name, status)
                })
                .collect()
        }

        /// Property keys the session says it supports
        /// (`VTSessionCopySupportedPropertyDictionary`).
        #[must_use]
        pub fn supported_properties(&self) -> Vec<String> {
            let ptr: NonNull<CFType> = NonNull::from(self.session.as_ref());
            // SAFETY: a `VTCompressionSessionRef` is a `VTSessionRef` (VTSession.h); read only.
            let session: &VTSession = unsafe { ptr.cast::<VTSession>().as_ref() };
            let mut raw: *const CFDictionary = ptr::null();
            // SAFETY: valid session and out pointer; the dictionary comes back +1 (Copy rule).
            let status = unsafe {
                VTSessionCopySupportedPropertyDictionary(session, NonNull::from(&mut raw))
            };
            let Some(raw) = NonNull::new(raw.cast_mut()) else {
                tracing::debug!(status, "no supported-property dictionary");
                return Vec::new();
            };
            // SAFETY: +1 reference from the copy call, keyed by `CFString`s (VTSession.h).
            let dict: CFRetained<CFDictionary<CFString, CFType>> =
                unsafe { CFRetained::from_raw(raw.cast()) };
            let (keys, _values) = dict.to_vecs();
            let mut names: Vec<String> = keys.iter().map(ToString::to_string).collect();
            names.sort_unstable();
            names
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::arithmetic_side_effects,
        reason = "test fixture arithmetic on small, bounded values"
    )]

    use objc2_core_media::{CMBlockBuffer, CMSampleTimingInfo, kCMBlockBufferAssureMemoryNowFlag};

    use super::*;

    /// A sample buffer holding `body` (length-prefixed NAL units) the way the encoder hands one
    /// over, without a format description or attachments.
    fn sample_of(body: &[u8]) -> CFRetained<CMSampleBuffer> {
        let mut block: *mut CMBlockBuffer = ptr::null_mut();
        // SAFETY: CoreMedia rule: a NULL memory block makes CoreMedia allocate `len` bytes.
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
        assert_eq!(status, 0);
        // SAFETY: +1 reference from the create call.
        let block = unsafe { CFRetained::from_raw(NonNull::new(block).unwrap()) };
        // SAFETY: the source holds `body.len()` bytes, the block exactly that capacity.
        let status = unsafe {
            CMBlockBuffer::replace_data_bytes(
                NonNull::from(&body[0]).cast::<c_void>(),
                &block,
                0,
                body.len(),
            )
        };
        assert_eq!(status, 0);
        let timing = CMSampleTimingInfo {
            duration: cf::time_us(0),
            presentationTimeStamp: cf::time_us(1),
            decodeTimeStamp: cf::time_us(1),
        };
        let size = body.len();
        let mut sample: *mut CMSampleBuffer = ptr::null_mut();
        // SAFETY: one sample, one timing entry, one size entry; every pointer is valid.
        let status = unsafe {
            CMSampleBuffer::create_ready(
                None,
                Some(&block),
                None,
                1,
                1,
                &raw const timing,
                1,
                &raw const size,
                NonNull::from(&mut sample),
            )
        };
        assert_eq!(status, 0);
        // SAFETY: +1 reference from the create call.
        unsafe { CFRetained::from_raw(NonNull::new(sample).unwrap()) }
    }

    /// Length-prefixed NAL units of `sizes` bytes each.
    fn access_unit(sizes: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        for (n, &size) in sizes.iter().enumerate() {
            out.extend_from_slice(&u32::try_from(size).unwrap().to_be_bytes());
            out.extend((0..size).map(|i| u8::try_from(i.wrapping_add(n) % 251).unwrap()));
        }
        out
    }

    const W: usize = 320;
    const H: usize = 180;

    /// A full-range NV12 frame, the format the worker captures, with a moving gradient.
    fn frame(index: usize) -> CFRetained<CVPixelBuffer> {
        use objc2_core_video::{
            CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
            CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress,
            CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        };
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: CoreVideo rule: a valid out-pointer and no attributes.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                W,
                H,
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                None,
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, 0);
        // SAFETY: +1 reference from the create call.
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) };
        // SAFETY: CoreVideo rule: lock before touching the planes.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(locked, 0);
        for plane in 0..2 {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            let rows = if plane == 0 { H } else { H / 2 };
            for y in 0..rows {
                // SAFETY: the plane is locked, so row `y < rows` starts inside its mapping.
                let start = unsafe { base.add(y * stride) };
                // SAFETY: the row spans `stride >= W` writable bytes of the locked plane.
                let row = unsafe { std::slice::from_raw_parts_mut(start, W) };
                for (x, cell) in row.iter_mut().enumerate() {
                    *cell = if plane == 0 {
                        u8::try_from((x + y + index * 7) % 256).unwrap()
                    } else {
                        128
                    };
                }
            }
        }
        // SAFETY: matches the lock above.
        let unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(unlocked, 0);
        buffer
    }

    fn encoder(tx: std::sync::mpsc::Sender<EncodedPacket>) -> Encoder {
        let config = EncoderConfig {
            width: u32::try_from(W).unwrap(),
            height: u32::try_from(H).unwrap(),
            codec: VideoCodec::Hevc,
            fps: 60,
            bitrate_bps: 2_000_000,
        };
        Encoder::new(config, move |packet| {
            let _receiver_gone = tx.send(packet);
        })
        .unwrap()
    }

    fn collect(rx: &std::sync::mpsc::Receiver<EncodedPacket>, n: usize) -> Vec<EncodedPacket> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut out = Vec::new();
        while out.len() < n {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok(p) => out.push(p),
                Err(_) => break,
            }
        }
        out
    }

    /// The refresh flag comes back on the frame that asked for it and on no other, however
    /// many frames are in flight around it.
    #[test]
    fn the_refresh_flag_rides_with_its_own_frame() {
        assert!(is_refresh(frame_refcon(true)));
        assert!(!is_refresh(frame_refcon(false)));
        let (tx, rx) = std::sync::mpsc::channel();
        let encoder = encoder(tx);
        if !encoder.ltr_enabled() {
            eprintln!("no LTR on this encoder; nothing to check");
            return;
        }
        let frames = 12_usize;
        for i in 0..frames {
            let options = FrameOptions {
                force_keyframe: i == 0,
                force_ltr_refresh: i == 7,
                acked_ltr: Vec::new(),
            };
            encoder.encode(&frame(i), u64::try_from(i).unwrap() * 16_667, &options).unwrap();
        }
        encoder.flush().unwrap();
        let packets = collect(&rx, frames);
        assert_eq!(packets.len(), frames);
        let flagged: Vec<u64> =
            packets.iter().filter(|p| p.ltr_refresh).map(|p| p.pts_us).collect();
        assert_eq!(flagged, vec![7 * 16_667], "only the frame that asked is a refresh");
    }

    /// The stream is full range and says BT.709 in its VUI, so the client's decoder outputs the
    /// captured format without a conversion pass and its shader reads the matrix off the frame.
    #[test]
    fn the_stream_is_full_range_bt709_end_to_end() {
        use objc2_core_media::{
            kCMFormatDescriptionExtension_FullRangeVideo, kCMFormatDescriptionExtension_YCbCrMatrix,
        };
        use objc2_core_video::{
            CVPixelBufferGetPixelFormatType, kCVImageBufferYCbCrMatrixKey,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let encoder = encoder(tx);
        for i in 0..3 {
            let options = FrameOptions { force_keyframe: i == 0, ..FrameOptions::default() };
            encoder.encode(&frame(i), u64::try_from(i).unwrap() * 16_667, &options).unwrap();
        }
        encoder.flush().unwrap();
        let packets = collect(&rx, 3);
        let keyframe = packets.first().filter(|p| p.keyframe).expect("a keyframe first");
        let unit = annexb::AccessUnit::parse(&keyframe.data, annexb::hevc::is_parameter_set);
        let sets: Vec<Vec<u8>> = unit.parameter_sets().iter().map(|s| s.to_vec()).collect();
        let format = crate::decoder::format_description(VideoCodec::Hevc, &sets).unwrap();
        // SAFETY: framework-provided constant string; the description is valid.
        let full = unsafe { format.extension(kCMFormatDescriptionExtension_FullRangeVideo) };
        // SAFETY: as above.
        let matrix = unsafe { format.extension(kCMFormatDescriptionExtension_YCbCrMatrix) };
        let full = full.and_then(|v| v.downcast::<CFBoolean>().ok()).map(|b| b.as_bool());
        assert_eq!(full, Some(true), "the VUI says full range");
        let matrix = matrix.and_then(|v| v.downcast::<CFString>().ok()).map(|m| m.to_string());
        // SAFETY: framework-provided constant string.
        let bt709 = unsafe { kCVImageBufferYCbCrMatrix_ITU_R_709_2 }.to_string();
        assert_eq!(matrix.as_deref(), Some(bt709.as_str()), "the VUI says BT.709");

        let (dtx, drx) = std::sync::mpsc::channel();
        let mut decoder = crate::Decoder::new(VideoCodec::Hevc, move |frame| {
            let image = frame.image.as_cv();
            let format = CVPixelBufferGetPixelFormatType(image);
            // SAFETY: framework-provided constant string; a null mode pointer is allowed.
            let matrix = unsafe { image.attachment(kCVImageBufferYCbCrMatrixKey, ptr::null_mut()) }
                .and_then(|v| v.downcast::<CFString>().ok())
                .map(|m| m.to_string());
            let _receiver_gone = dtx.send((format, matrix));
        });
        for p in &packets {
            decoder.decode(&p.data, p.pts_us).unwrap();
        }
        let (format, matrix) =
            drx.recv_timeout(std::time::Duration::from_secs(60)).expect("a decoded frame");
        assert_eq!(format, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
        assert_eq!(matrix.as_deref(), Some(bt709.as_str()), "the frame carries the matrix");
    }

    /// What turning VideoToolbox's output into a packet costs on its callback thread: a 62 KB
    /// P-frame and a 300 KB keyframe in four slices. `docs/MEASUREMENTS.md` records runs.
    #[test]
    #[ignore = "a measurement; run with --run-ignored only --no-capture in release"]
    fn packet_conversion_cost() {
        for (name, len) in [("P-frame", 62_000_usize), ("keyframe", 300_000)] {
            let body = access_unit(&[len / 4; 4]);
            let sample = sample_of(&body);
            let rounds = 2_000_u32;
            let started = std::time::Instant::now();
            let mut bytes = 0_usize;
            for _ in 0..rounds {
                bytes = bytes
                    .wrapping_add(packet(&sample, VideoCodec::Hevc, false).unwrap().data.len());
            }
            let per = started.elapsed() / rounds;
            eprintln!("{name} {len} B: {per:?} per packet ({} B out)", bytes / rounds as usize);
        }
    }
}

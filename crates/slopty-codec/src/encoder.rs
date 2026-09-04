//! `VTCompressionSession` for interactive streaming (macOS host only).

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, Type as _,
};
use objc2_core_media::{
    CMBlockBuffer, CMSampleBuffer, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMSampleAttachmentKey_NotSync,
    kCMTimeInvalid, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC,
};
use objc2_core_video::CVPixelBuffer;
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, kVTCompressionPropertyKey_AllowFrameReordering,
    kVTCompressionPropertyKey_AllowOpenGOP, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_DataRateLimits, kVTCompressionPropertyKey_EnableLTR,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxFrameDelayCount,
    kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaximumRealTimeFrameRate,
    kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTEncodeFrameOptionKey_AcknowledgedLTRTokens, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTEncodeFrameOptionKey_ForceLTRRefresh, kVTProfileLevel_H264_High_AutoLevel,
    kVTProfileLevel_HEVC_Main_AutoLevel, kVTProfileLevel_HEVC_Main10_AutoLevel,
    kVTPropertyNotSupportedErr, kVTSampleAttachmentKey_RequireLTRAcknowledgementToken,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};
use slopty_proto::screen::VideoCodec;

use crate::cf::{self, check};
use crate::{CodecError, annexb};

/// Encoder settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EncoderConfig {
    /// Pixel width (even).
    pub width: u32,
    /// Pixel height (even).
    pub height: u32,
    /// Codec.
    pub codec: VideoCodec,
    /// Expected frame rate; drives the rate controller's window.
    pub fps: u16,
    /// Target bitrate, bits per second.
    pub bitrate_bps: u32,
}

/// Per-frame requests.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FrameOptions {
    /// Encode an IDR.
    pub force_keyframe: bool,
    /// Encode a P-frame from an acknowledged long-term reference (an IDR if none is acked).
    pub force_ltr_refresh: bool,
    /// LTR tokens the receiver has acknowledged since the last frame.
    pub acked_ltr: Vec<u64>,
}

/// One encoded access unit, Annex B, parameter sets inline before keyframes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EncodedPacket {
    /// Bitstream.
    pub data: Vec<u8>,
    /// IDR.
    pub keyframe: bool,
    /// Token the receiver must acknowledge for this frame to become a usable LTR.
    pub ltr_token: Option<u64>,
    /// Produced in answer to a `force_ltr_refresh`.
    pub ltr_refresh: bool,
    /// The presentation timestamp passed to `encode`.
    pub pts_us: u64,
}

type Sink = Box<dyn Fn(EncodedPacket) + Send + Sync>;

fn retain(value: &CFType) -> CFRetained<CFType> {
    CFType::retain(value)
}

struct Shared {
    sink: Sink,
    codec: VideoCodec,
    pending_refresh: AtomicBool,
}

/// A hardware encoder.
pub struct Encoder {
    session: CFRetained<VTCompressionSession>,
    shared: Arc<Shared>,
    config: EncoderConfig,
    ltr: bool,
}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder")
            .field("config", &self.config)
            .field("ltr", &self.ltr)
            .finish_non_exhaustive()
    }
}

impl Encoder {
    /// Create and configure a session. Packets are delivered to `sink` on VideoToolbox's thread.
    pub fn new(
        config: EncoderConfig,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        let shared = Arc::new(Shared {
            sink: Box::new(sink),
            codec: config.codec,
            pending_refresh: AtomicBool::new(false),
        });
        let spec = CFDictionary::<CFString, CFType>::from_slices(
            &[
                // SAFETY: framework-provided constant string.
                unsafe { kVTVideoEncoderSpecification_EnableLowLatencyRateControl },
                // SAFETY: framework-provided constant string.
                unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder },
            ],
            &[cf::boolean(true), cf::boolean(true)],
        );
        let codec_type = match config.codec {
            VideoCodec::Hevc | VideoCodec::HevcMain10 => kCMVideoCodecType_HEVC,
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
        let mut encoder = Self { session, shared, config, ltr: false };
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

    /// Set a property that some encoders do not implement; unsupported is logged, not fatal.
    fn set_optional(&self, key: &CFString, value: &CFType, call: &'static str) -> bool {
        match self.set(key, value, call) {
            Ok(()) => true,
            Err(CodecError::Os { status, .. }) if status == kVTPropertyNotSupportedErr => {
                tracing::warn!(call, "encoder does not support this property");
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
        let table: [(&CFString, CFRetained<CFType>, &'static str, bool); 8] = [
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
            VideoCodec::HevcMain10 => unsafe { kVTProfileLevel_HEVC_Main10_AutoLevel },
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

    /// Change the target bitrate on the fly (bits per second).
    pub fn set_bitrate(&self, bps: u32) -> Result<(), CodecError> {
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
        if options.force_ltr_refresh && self.ltr {
            // SAFETY: framework-provided constant string.
            let key = unsafe { kVTEncodeFrameOptionKey_ForceLTRRefresh };
            keys.push(key);
            values.push(CFBoolean::new(true));
            self.shared.pending_refresh.store(true, Ordering::Release);
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
        // session retains it as long as the encoder needs it. No source refcon is used.
        let status = unsafe {
            self.session.encode_frame(
                image,
                cf::time_us(pts_us),
                kCMTimeInvalid,
                properties.as_deref().map(CFDictionary::as_opaque),
                ptr::null_mut(),
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

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: invalidation stops callbacks; afterwards the session never touches the
        // refcon again, so the `Arc<Shared>` may be released with `self`.
        unsafe { self.session.invalidate() }
    }
}

unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    _source: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: the refcon was created from `Arc::as_ptr` on the encoder's `Shared`, which the
    // `Encoder` keeps alive until the session is invalidated (see `Drop`).
    let shared: &Shared = unsafe { &*refcon.cast::<Shared>() };
    let refresh = shared.pending_refresh.swap(false, Ordering::AcqRel);
    if status != 0 || flags.contains(VTEncodeInfoFlags::FrameDropped) {
        tracing::debug!(status, ?flags, "encoder dropped a frame");
        if refresh {
            shared.pending_refresh.store(true, Ordering::Release);
        }
        return;
    }
    let Some(sample) = NonNull::new(sample) else { return };
    // SAFETY: the sample buffer is valid for the duration of the callback.
    let sample: &CMSampleBuffer = unsafe { sample.as_ref() };
    if let Some(packet) = packet(sample, shared.codec, refresh) {
        (shared.sink)(packet);
    } else {
        tracing::warn!("encoder produced a sample without readable data");
    }
}

fn packet(sample: &CMSampleBuffer, codec: VideoCodec, refresh: bool) -> Option<EncodedPacket> {
    // SAFETY: valid sample buffer.
    let block = unsafe { sample.data_buffer() }?;
    let body = block_bytes(&block)?;
    let (keyframe, ltr_token) = attachments(sample);
    // SAFETY: valid sample buffer.
    let pts_us = cf::micros(unsafe { sample.presentation_time_stamp() }).unwrap_or(0);
    let mut data = Vec::with_capacity(body.len().saturating_add(256));
    let mut nal_length = 4;
    if keyframe {
        // SAFETY: valid sample buffer.
        if let Some(format) = unsafe { sample.format_description() } {
            let (sets, len) = parameter_sets(&format, codec);
            nal_length = len;
            annexb::prepend_parameter_sets(&mut data, sets.iter().map(Vec::as_slice));
        }
    }
    data.extend(annexb::length_prefixed_to_annexb(&body, nal_length));
    Some(EncodedPacket { data, keyframe, ltr_token, ltr_refresh: refresh, pts_us })
}

fn block_bytes(block: &CMBlockBuffer) -> Option<Vec<u8>> {
    // SAFETY: valid block buffer.
    let len = unsafe { block.data_length() };
    let mut out = vec![0_u8; len];
    let first = out.first_mut()?;
    // SAFETY: the destination has `len` writable bytes, exactly the block's data length.
    let status = unsafe { block.copy_data_bytes(0, len, NonNull::from(first).cast::<c_void>()) };
    (status == 0).then_some(out)
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

/// The parameter sets of a format description plus its NAL length size.
fn parameter_sets(
    format: &objc2_core_media::CMFormatDescription,
    codec: VideoCodec,
) -> (Vec<Vec<u8>>, usize) {
    let mut sets = Vec::new();
    let mut count: usize = 0;
    let mut nal_length: std::ffi::c_int = 4;
    let mut index = 0_usize;
    loop {
        let mut ptr: *const u8 = ptr::null();
        let mut size: usize = 0;
        let status = match codec {
            // SAFETY: every out-pointer is valid; the parameter set bytes are owned by the
            // format description, which outlives this function.
            VideoCodec::Hevc | VideoCodec::HevcMain10 => unsafe {
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
        // SAFETY: CoreMedia returned `size` readable bytes at `ptr`.
        sets.push(unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec());
        index = index.saturating_add(1);
        if index >= count {
            break;
        }
    }
    (sets, usize::try_from(nal_length).unwrap_or(4))
}

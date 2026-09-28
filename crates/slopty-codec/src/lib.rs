//! Hardware video codecs behind a small, callback-driven API.
//!
//! * [`annexb`] — pure NAL unit framing, used by both halves and on every platform.
//! * [`video`] — the worker's encoder seam, [`VideoEncoder`] and [`AudioEncoder`], on every
//!   platform; `VideoToolbox` and `Opus` implement it on macOS.
//! * `Encoder` (macOS) — a `VTCompressionSession` tuned for interactive streaming: low-latency rate
//!   control, no reordering, infinite GOP with long-term references, Annex B out.
//! * `Decoder` (macOS, iOS) — a `VTDecompressionSession` fed Annex B; it rebuilds its format
//!   description from the parameter sets in front of each keyframe.
//! * [`audio`] — Opus through `AudioConverter` (encode on the worker, decode anywhere) and a player
//!   that renders from its jitter ring on the output unit's I/O thread.
//!
//! Output is delivered on VideoToolbox's own threads through the sink closure given at
//! construction; sinks must be cheap (hand the packet to a channel).

pub mod annexb;
#[cfg(target_vendor = "apple")]
pub mod audio;
pub mod video;

#[cfg(target_vendor = "apple")]
mod cf;
#[cfg(target_vendor = "apple")]
mod decoder;
#[cfg(target_os = "macos")]
mod encoder;

#[cfg(target_vendor = "apple")]
pub use audio::Opus;
#[cfg(target_vendor = "apple")]
pub use cf::micros;
#[cfg(target_vendor = "apple")]
pub use decoder::{
    DecodeFailure, DecodeOutcome, DecodedFrame, Decoder, PixelBuffer, session_lost, warm_up,
};
#[cfg(all(target_os = "macos", feature = "experiments"))]
pub use encoder::RateControl;
#[cfg(target_os = "macos")]
pub use encoder::{Encoder, VideoToolbox, pixel_format};
pub use video::{AudioEncoder, Chroma, EncodedPacket, EncoderConfig, FrameOptions, VideoEncoder};

/// Codec failures. The `OSStatus` codes are VideoToolbox's (`kVT*Err`, negative).
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum CodecError {
    /// A `VideoToolbox` or `CoreMedia` call failed.
    #[error("{call} failed: OSStatus {status}")]
    Os {
        /// The API that failed.
        call: &'static str,
        /// The status code.
        status: i32,
    },
    /// The bitstream has no parameter sets and no decoder exists yet.
    #[error("no parameter sets seen yet; waiting for a keyframe")]
    NoParameterSets,
    /// An access unit whose NAL lengths do not add up.
    #[error("malformed NAL unit length at byte {offset}")]
    MalformedNal {
        /// Where the bad length starts.
        offset: usize,
    },
    /// The codec is not supported on this platform.
    #[error("unsupported codec {0:?}")]
    Unsupported(slopty_proto::screen::VideoCodec),
    /// The encoder offers no 4:4:4 profile for this codec.
    #[error("no 4:4:4 encoder for {0:?}")]
    NoFullChroma(slopty_proto::screen::VideoCodec),
    /// A 4:4:4 session was handed a picture in another pixel format (the four-character code);
    /// the hardware would have encoded it as 4:2:0.
    #[error("a 4:4:4 session needs xf44 pictures, got {0:#010x}")]
    NotFullChroma(u32),
    /// This platform has no audio encoder (`docs/decisions/platform.md`, "Linux seams").
    #[error("no audio encoder on this platform")]
    NoAudio,
}

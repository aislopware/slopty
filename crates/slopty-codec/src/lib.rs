//! Hardware video codecs behind a small, callback-driven API.
//!
//! * [`annexb`] — pure NAL unit framing, used by both halves and on every platform.
//! * [`conformance`] — the SPS conformance window that shows a picture coded at a padded size at
//!   its true one.
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

// RealtimeSanitizer's attribute on the audio render callback, in the nightly deep lane only
// (`cargo xtask deep sanitize-realtime`).
#![cfg_attr(slopty_rtsan, feature(sanitize))]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod annexb;
#[cfg(target_vendor = "apple")]
pub mod audio;
pub mod conformance;
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
pub use video::{
    AudioEncoder, Chroma, EncodedPacket, EncoderConfig, FrameOptions, Mse, VideoEncoder,
};

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
    /// A session was handed a picture of another size than its own: a capture from before a
    /// resize, which the encoder would have coded into the session's size.
    #[error("a {session:?} session was handed a {image:?} picture")]
    WrongSize {
        /// The picture's width and height.
        image: (usize, usize),
        /// The session's.
        session: (usize, usize),
    },
    /// This platform has no audio encoder (`docs/decisions/platform.md`, "Linux seams").
    #[error("no audio encoder on this platform")]
    NoAudio,
}

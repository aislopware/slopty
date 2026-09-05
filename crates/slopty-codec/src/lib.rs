//! Hardware video codecs behind a small, callback-driven API.
//!
//! * [`annexb`] — pure NAL unit framing, used by both halves and on every platform.
//! * `Encoder` (macOS) — a `VTCompressionSession` tuned for interactive streaming: low-latency rate
//!   control, no reordering, infinite GOP with long-term references, Annex B out.
//! * `Decoder` (macOS, iOS) — a `VTDecompressionSession` fed Annex B; it rebuilds its format
//!   description from the parameter sets in front of each keyframe.
//! * [`audio`] — Opus through `AudioConverter` (encode on the host, decode anywhere) and an
//!   `AudioQueue` player.
//!
//! Output is delivered on VideoToolbox's own threads through the sink closure given at
//! construction; sinks must be cheap (hand the packet to a channel).

pub mod annexb;
#[cfg(target_vendor = "apple")]
pub mod audio;

#[cfg(target_vendor = "apple")]
mod cf;
#[cfg(target_vendor = "apple")]
mod decoder;
#[cfg(target_os = "macos")]
mod encoder;

#[cfg(target_vendor = "apple")]
pub use decoder::{DecodedFrame, Decoder, PixelBuffer, warm_up};
#[cfg(target_os = "macos")]
pub use encoder::{EncodedPacket, Encoder, EncoderConfig, FrameOptions, RateControl};

/// Codec failures. The `OSStatus` codes are VideoToolbox's (`kVT*Err`, negative).
#[derive(Debug, thiserror::Error)]
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
    /// The codec is not supported on this platform.
    #[error("unsupported codec {0:?}")]
    Unsupported(slopty_proto::screen::VideoCodec),
}

//! Media transport policy, pure and fuzzable.
//!
//! The worker side turns each encoded video frame into ≤ 1200-byte datagrams with a fixed
//! [`slopty_proto::media::MediaHeader`], adds systematic Reed–Solomon parity per frame
//! ([`Packetizer`]) and answers NACKs from a short send history. The client side puts the
//! fragments back together, recovers from parity or retransmission, delivers frames strictly in
//! decode order and decides when to NACK, when to give up on a frame and ask for an LTR refresh,
//! and what to report back ([`Reassembler`]). [`Redundancy`] closes the loop by turning receiver
//! reports into a parity ratio.
//!
//! Nothing here touches sockets, codecs or clocks: callers pass `now` and ship the bytes.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

mod cursor;
mod heartbeat;
mod layers;
mod packetize;
mod rate;
mod reassemble;
mod redundancy;
mod refine;

pub use cursor::{cursor_datagram, parse_cursor};
pub use heartbeat::{HEARTBEAT_AFTER, heartbeat_datagram};
pub use layers::LayerGate;
pub use packetize::{
    EncodedFrame, Layout, MAX_DATA_FRAGMENTS, MAX_PARITY_FRAGMENTS, MIN_PARITY_FRAGMENTS,
    Packetizer, SentFrame, audio_datagram, layout,
};
pub use rate::{
    Cadence, ChromaGate, Decision, EncoderWatch, FULL_CHROMA_ENTER_BPS, FULL_CHROMA_HOLD,
    FULL_CHROMA_LEAVE_BPS, Fed, Pace, PathSample, RateController, Window as RateWindow,
    full_chroma_band, judge, slower_rung,
};
pub use reassemble::{
    Action, Config, FrameInfo, FrameOut, Ignored, Ingest, NackDelay, Reassembler, ReassemblerStats,
    STALL_GAP, StallAttribution,
};
pub use redundancy::Redundancy;
pub use refine::{IDLE_SHARE, MAX_REFINEMENTS, PERIODS_APART, Refine};

/// Errors from packetizing.
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum MediaError {
    /// The frame has no bytes.
    #[error("empty frame")]
    Empty,
    /// The frame needs more fragments than a header can address.
    #[error("frame of {len} bytes needs more than {MAX_DATA_FRAGMENTS} fragments")]
    FrameTooLarge {
        /// Frame bytes.
        len: usize,
    },
    /// The Reed–Solomon engine refused the configuration.
    #[error("reed-solomon: {0}")]
    Fec(#[from] reed_solomon_simd::Error),
}

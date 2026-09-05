//! Media transport policy, pure and fuzzable.
//!
//! The host side turns each encoded video frame into ≤ 1200-byte datagrams with a fixed
//! [`slopty_proto::media::MediaHeader`], adds systematic Reed–Solomon parity per frame
//! ([`Packetizer`]) and answers NACKs from a short send history. The client side puts the
//! fragments back together, recovers from parity or retransmission, delivers frames strictly in
//! decode order and decides when to NACK, when to give up on a frame and ask for an LTR refresh,
//! and what to report back ([`Reassembler`]). [`Redundancy`] closes the loop by turning receiver
//! reports into a parity ratio.
//!
//! Nothing here touches sockets, codecs or clocks: callers pass `now` and ship the bytes.

#![forbid(unsafe_code)]

mod cursor;
mod heartbeat;
mod packetize;
mod rate;
mod reassemble;
mod redundancy;

pub use cursor::{cursor_datagram, parse_cursor};
pub use heartbeat::{HEARTBEAT_AFTER, heartbeat_datagram};
pub use packetize::{
    DEFAULT_PARITY_PERMILLE, EncodedFrame, HISTORY_FRAMES, Layout, MAX_DATA_FRAGMENTS,
    MAX_PARITY_FRAGMENTS, MIN_PAYLOAD, Packetizer, SentFrame, audio_datagram, layout,
};
pub use rate::{Decision, PathSample, RateController, Window as RateWindow, judge};
pub use reassemble::{
    Action, Config, FrameInfo, FrameOut, Ignored, Ingest, NackDelay, Reassembler, ReassemblerStats,
    STALL_GAP, StallAttribution,
};
pub use redundancy::Redundancy;

/// Errors from packetizing.
#[derive(Debug, thiserror::Error)]
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

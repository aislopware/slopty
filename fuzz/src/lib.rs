//! What each fuzz target does with its input, as plain functions.
//!
//! Every decoder a peer's bytes reach has a target: the control and unidirectional streams
//! through the real framing ([`slopty_proto::codec::try_take`], as `slopty_net::framed` reads
//! them), each kind of datagram, the media reassembler and its forward error correction, the
//! worker's side of a client's loss feedback, a video frame's NAL unit framing and SPS reads,
//! and the worker's control socket. `fuzz_targets/` hands each function libFuzzer's input;
//! `tests/regressions.rs` replays every input kept under `regressions/<target>/` through the
//! same function on an ordinary build, so a fixed crash stays fixed.
//!
//! A target fails by panicking: on a crash in the code under test, or on an invariant the
//! decoded value breaks (it must encode back to the same bytes, and the fast paths that read a
//! message's head must agree with the full decode). A decode must also take no more heap than
//! its bytes bound, plus the cells its terminal lines took from the decode's cell budget
//! ([`stream`]'s `decode`), counted by the allocator the decoding targets install.

pub mod datagram;
pub mod feedback;
pub mod nal;
pub mod reassemble;
pub mod stream;

/// One fuzz target: its name (the file under `fuzz_targets/` and its directory under
/// `regressions/`) and what it does with an input.
pub type Target = (&'static str, fn(&[u8]));

/// Every target.
pub const TARGETS: &[Target] = &[
    ("client_msg", stream::client_msg),
    ("worker_msg", stream::worker_msg),
    ("server_msg", stream::server_msg),
    ("uni_stream", stream::uni_stream),
    ("client_datagram", datagram::client_datagram),
    ("term_datagram", datagram::term_datagram),
    ("media_header", datagram::media_header),
    ("cursor", datagram::cursor),
    ("origin", datagram::origin),
    ("reassemble", reassemble::run),
    ("feedback", feedback::run),
    ("ctl", stream::ctl),
    ("nal", nal::run),
];

/// The target named `name`.
#[must_use]
pub fn target(name: &str) -> Option<fn(&[u8])> {
    TARGETS.iter().find(|(n, _)| *n == name).map(|(_, run)| *run)
}

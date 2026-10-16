//! Reports the server sends an agent (`docs/decisions/projects.md`).
//!
//! `slopty-server` frames each batch in [`TAG`] and hands it to the driven agent as a message of
//! its own, and the decoder reads a message so framed as Slopty's notice, not the person's words
//! ([`slopty_proto::thread::Notice::REPORT`]).

/// The tag a batch of reports is framed in.
pub const TAG: &str = "slopty-reports";

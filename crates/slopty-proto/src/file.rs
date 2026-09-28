//! A file read for a file tile: the worker reads it whole and says what it found; and how the
//! tile's save ended.
//!
//! A text that fits [`INLINE_FILE_BYTES`] rides the control stream in [`FileRead::Text`]. A
//! larger one is announced there as [`FileRead::Streamed`] and its bytes follow on a bulk stream
//! ([`crate::transfer::Purpose::FileText`]) named by the same transfer, so the control stream
//! never carries more than a clipboard's worth of text. A save goes back the same two ways:
//! [`crate::ClientMsg::WriteFile`] inline, or a bulk stream ([`crate::transfer::Purpose::Save`]).

use serde::{Deserialize, Serialize};
use slopty_core::{WallMs, XferId};

/// Bytes a file tile edits at most: a file larger than this is [`FileRead::TooLarge`], and a
/// save of more is refused.
pub const FILE_BYTES: u64 = 16 << 20;

/// A text at most this big goes inline on the control stream, where it queues ahead of input
/// and acks; anything bigger goes on a bulk stream. The clipboard's inline limit, for the
/// same reason.
pub const INLINE_FILE_BYTES: usize = crate::transfer::INLINE_CLIP_BYTES;

/// What the worker found at a path.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FileRead {
    /// A text file, whole.
    Text {
        /// The file's text, without its final newline.
        text: String,
        /// Size on disk, bytes.
        size: u64,
        /// Last modification.
        modified_ms: WallMs,
        /// The file ends with a newline, which `text` leaves off; a save puts it back.
        final_newline: bool,
    },
    /// Not text (a NUL byte, or not UTF-8).
    Binary {
        /// Size on disk, bytes.
        size: u64,
    },
    /// Nothing readable at the path: missing, a directory, or not permitted.
    Missing {
        /// The OS's word for it.
        error: String,
    },
    /// Larger than [`FILE_BYTES`]: nothing was read.
    TooLarge {
        /// Size on disk, bytes.
        size: u64,
    },
    /// A text file larger than [`INLINE_FILE_BYTES`]: `text` of [`FileRead::Text`] follows on
    /// the bulk stream of transfer `xfer`. A client's link joins the two and hands its owner the
    /// [`FileRead::Text`] they make; a later read of the same path supersedes one still on
    /// its way.
    Streamed {
        /// The bulk stream the text arrives on.
        xfer: XferId,
        /// Size on disk, bytes.
        size: u64,
        /// Last modification.
        modified_ms: WallMs,
        /// The file ends with a newline, which the streamed text leaves off.
        final_newline: bool,
    },
}

/// How a [`crate::ClientMsg::WriteFile`] ended.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum WriteResult {
    /// Written whole (a temporary file renamed over the old one).
    Saved {
        /// Size on disk now, bytes.
        size: u64,
        /// Modification time now.
        modified_ms: WallMs,
    },
    /// The file changed on disk since the version the edit started from; nothing was written.
    Conflict {
        /// Its modification time on disk.
        modified_ms: WallMs,
    },
    /// Not written.
    Failed {
        /// The OS's word for it.
        error: String,
    },
}

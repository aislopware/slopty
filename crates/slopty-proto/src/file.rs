//! A file read for a file card: the worker reads it, clips it and says what it found.

use serde::{Deserialize, Serialize};

/// Lines a file card carries at most; the rest is counted.
pub const FILE_LINES: u32 = 2000;
/// Bytes the worker reads of a file at most, before the line clip.
pub const FILE_BYTES: u64 = 512 * 1024;

/// What the worker found at a path.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FileRead {
    /// A text file, the first [`FILE_LINES`] lines of it.
    Text {
        /// The kept lines, joined by `\n`, without a trailing newline.
        text: String,
        /// Lines dropped after `text`; zero when nothing was cut.
        more_lines: u32,
        /// Size on disk, bytes.
        size: u64,
        /// Last modification, milliseconds since the Unix epoch.
        modified_ms: u64,
    },
    /// Not text (a NUL byte, or not UTF-8 in what was read).
    Binary {
        /// Size on disk, bytes.
        size: u64,
    },
    /// Nothing readable at the path: missing, a directory, or not permitted.
    Missing {
        /// The OS's word for it.
        error: String,
    },
}

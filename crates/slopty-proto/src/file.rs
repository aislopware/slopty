//! A file read for a file tile: the worker reads it whole and says what it found; and how the
//! tile's save ended.
//!
//! A text is edited ([`FileRead::Text`]); a picture or a PDF is shown ([`FileRead::Media`]), its
//! bytes as the file has them, so the client's platform decodes it at the size it draws. Either
//! one that fits [`INLINE_FILE_BYTES`] rides the control stream. A larger one is announced there
//! as [`FileRead::Streamed`] and its bytes follow on a bulk stream
//! ([`crate::transfer::Purpose::FileBody`]) named by the same transfer, so the control stream
//! never carries more than a clipboard's worth. A save goes back the same two ways:
//! [`crate::ClientMsg::WriteFile`] inline, or a bulk stream ([`crate::transfer::Purpose::Save`]).

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use slopty_core::{WallMs, XferId};

/// Bytes a file tile edits at most: a file larger than this is [`FileRead::TooLarge`], and a
/// save of more is refused.
pub const FILE_BYTES: u64 = 16 << 20;

/// Bytes a file tile shows at most as a picture or a document.
///
/// A picture or document past this is [`FileRead::Binary`]. Above [`FILE_BYTES`], since a
/// scanned PDF or a camera's raw picture is larger than any text worth editing, and bounded,
/// since the client holds it whole to draw it.
pub const MEDIA_BYTES: u64 = 128 << 20;

/// A read at most this big goes inline on the control stream, where it queues ahead of input
/// and acks; anything bigger goes on a bulk stream. The clipboard's inline limit, for the
/// same reason.
pub const INLINE_FILE_BYTES: usize = crate::transfer::INLINE_CLIP_BYTES;

/// A text file's `EditorConfig` properties, resolved on the worker.
///
/// Each `key = value` the `.editorconfig` files above the file set for it
/// (<https://editorconfig.org>), in the order they apply. Keys are lowercased, and so are the
/// values of the keys the specification defines.
///
/// The list is open: the client acts on the keys it knows (`indent_style`, `indent_size`,
/// `tab_width`, `end_of_line`, `insert_final_newline`, `trim_trailing_whitespace`) and passes
/// over the rest. Empty when no `.editorconfig` applies.
pub type EditorConfig = Vec<(String, String)>;

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
        /// The file's [`EditorConfig`] properties.
        editorconfig: EditorConfig,
    },
    /// Not text (a NUL byte, or not UTF-8), and not a picture or document the tile shows.
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
    /// A text or media file larger than [`INLINE_FILE_BYTES`]: its bytes follow on the bulk
    /// stream of transfer `xfer`. A client's link joins the two and hands its owner the
    /// [`FileRead::Text`] or [`FileRead::Media`] they make; a later read of the same path
    /// supersedes one still on its way.
    Streamed {
        /// The bulk stream the bytes arrive on.
        xfer: XferId,
        /// Size on disk, bytes.
        size: u64,
        /// Last modification.
        modified_ms: WallMs,
        /// What the bytes are.
        body: Body,
    },
    /// A file the tile shows rather than edits, whole: a picture or a document, known by its
    /// first bytes, not its name.
    Media {
        /// What it is, as a media type: `image/png`, `image/heic`, `application/pdf`. The client
        /// shows what its platform can draw and says so of the rest.
        media_type: String,
        /// The file's bytes, as on disk.
        bytes: Bytes,
        /// Last modification.
        modified_ms: WallMs,
    },
}

/// What a [`FileRead::Streamed`]'s bytes make once they are all here.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Body {
    /// A [`FileRead::Text`]: UTF-8, without the final newline.
    Text {
        /// The file ends with a newline, which the streamed text leaves off.
        final_newline: bool,
        /// As the [`FileRead::Text`]'s.
        editorconfig: EditorConfig,
    },
    /// A [`FileRead::Media`] of this media type.
    Media {
        /// As the `media_type` of [`FileRead::Media`].
        media_type: String,
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

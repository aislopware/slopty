//! The host's general pasteboard, for clipboard sync with remote-window clients.
//!
//! `NSPasteboard` has no change notification; the only signal is `changeCount`, so a
//! [`Pasteboard`] is polled (see [`Pasteboard::poll`]). Writes made through this type bump
//! the count too; they are remembered so the poller does not echo a client's own text back
//! to it. Plain text is polled; a picture is written when a client pushes one ahead of a
//! paste, never read back: files and rich text stay local.

use std::sync::atomic::{AtomicIsize, Ordering};

use objc2_app_kit::{
    NSPasteboard, NSPasteboardType, NSPasteboardTypePNG, NSPasteboardTypeString,
    NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSData, NSString};
pub use slopty_proto::screen::{MAX_CLIPBOARD_BYTES, MAX_CLIPBOARD_IMAGE_BYTES};

/// A picture encoding the pasteboard holds as it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PictureKind {
    /// `image/png`.
    Png,
    /// `image/tiff`, what a Mac screenshot is on the pasteboard.
    Tiff,
    /// `image/jpeg`.
    Jpeg,
}

impl PictureKind {
    /// The kind named by a media type; `None` for anything the pasteboard would have to
    /// convert (`image/webp`, `image/gif`, `image/svg+xml`).
    #[must_use]
    pub fn from_media_type(media_type: &str) -> Option<Self> {
        match media_type {
            "image/png" => Some(Self::Png),
            "image/tiff" => Some(Self::Tiff),
            "image/jpeg" => Some(Self::Jpeg),
            _ => None,
        }
    }

    /// Write `data` to `board` under the type this kind is held as (the UTI, as AppKit
    /// names it).
    fn set(self, board: &NSPasteboard, data: &NSData) -> bool {
        // `setData:forType:` copies the data into the pasteboard server.
        let kind: &NSPasteboardType = match self {
            // SAFETY: `NSPasteboardTypePNG` is an AppKit constant, valid for the process
            // lifetime.
            Self::Png => unsafe { NSPasteboardTypePNG },
            // SAFETY: `NSPasteboardTypeTIFF` is an AppKit constant, valid for the process
            // lifetime.
            Self::Tiff => unsafe { NSPasteboardTypeTIFF },
            Self::Jpeg => {
                return board.setData_forType(Some(data), &NSString::from_str("public.jpeg"));
            }
        };
        board.setData_forType(Some(data), kind)
    }
}

/// The general pasteboard plus the change count of the last write made through it.
#[derive(Debug)]
pub struct Pasteboard {
    /// `changeCount` after the last [`Pasteboard::poll`] or [`Pasteboard::write`].
    seen: AtomicIsize,
}

impl Default for Pasteboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Pasteboard {
    /// Start watching; whatever is on the pasteboard now is not reported.
    #[must_use]
    pub fn new() -> Self {
        let count = NSPasteboard::generalPasteboard().changeCount();
        Self { seen: AtomicIsize::new(count) }
    }

    /// Current `changeCount`.
    #[must_use]
    pub fn change_count() -> isize {
        NSPasteboard::generalPasteboard().changeCount()
    }

    /// New text since the last poll or write, if the pasteboard changed and now holds text
    /// that fits [`MAX_CLIPBOARD_BYTES`]. A change to something other than text (an image, a
    /// file) is consumed silently.
    pub fn poll(&self) -> Option<String> {
        let board = NSPasteboard::generalPasteboard();
        let count = board.changeCount();
        if count == self.seen.swap(count, Ordering::AcqRel) {
            return None;
        }
        // SAFETY: `NSPasteboardTypeString` is an AppKit constant, valid for the process
        // lifetime; `stringForType:` copies the string out of the pasteboard server.
        let text = board.stringForType(unsafe { NSPasteboardTypeString })?.to_string();
        (text.len() <= MAX_CLIPBOARD_BYTES).then_some(text)
    }

    /// Replace the pasteboard with `text`. The resulting change is not reported by
    /// [`Pasteboard::poll`].
    pub fn write(&self, text: &str) -> bool {
        let board = NSPasteboard::generalPasteboard();
        let _previous = board.clearContents();
        // SAFETY: as in `poll`, the type constant is a process-lifetime AppKit static.
        let ok =
            board.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
        self.seen.store(board.changeCount(), Ordering::Release);
        ok
    }

    /// Replace the pasteboard with a picture. The resulting change is not reported by
    /// [`Pasteboard::poll`]; a kind the pasteboard would have to convert, or bytes over
    /// [`MAX_CLIPBOARD_IMAGE_BYTES`], are refused.
    pub fn write_picture(&self, media_type: &str, bytes: &[u8]) -> bool {
        let Some(kind) = PictureKind::from_media_type(media_type) else { return false };
        if bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
            return false;
        }
        let board = NSPasteboard::generalPasteboard();
        let _previous = board.clearContents();
        let ok = kind.set(&board, &NSData::with_bytes(bytes));
        self.seen.store(board.changeCount(), Ordering::Release);
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kinds_the_pasteboard_holds_as_they_are() {
        assert_eq!(PictureKind::from_media_type("image/png"), Some(PictureKind::Png));
        assert_eq!(PictureKind::from_media_type("image/tiff"), Some(PictureKind::Tiff));
        assert_eq!(PictureKind::from_media_type("image/jpeg"), Some(PictureKind::Jpeg));
        assert_eq!(PictureKind::from_media_type("image/webp"), None, "would need converting");
        assert_eq!(PictureKind::from_media_type("text/plain"), None);
    }
}

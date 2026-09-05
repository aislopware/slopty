//! The host's general pasteboard, for clipboard sync with remote-window clients.
//!
//! `NSPasteboard` has no change notification; the only signal is `changeCount`, so a
//! [`Pasteboard`] is polled (see [`Pasteboard::poll`]). Writes made through this type bump
//! the count too; they are remembered so the poller does not echo a client's own text back
//! to it. Plain text only: files, images and rich text stay local.

use std::sync::atomic::{AtomicIsize, Ordering};

use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;
pub use slopty_proto::screen::MAX_CLIPBOARD_BYTES;

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
}

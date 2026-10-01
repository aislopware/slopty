//! Seeing a drag begin in an app on the worker: the drag pasteboard's change count.
//!
//! The worker holds the button down for a client, so it knows when a press on a stream may turn
//! into a drag. It marks the drag pasteboard's change count at the press and looks again as the
//! pointer moves: a click leaves the count where it was, and a drag that begins moves it (P0
//! (5)). Then the pasteboard holds what the drag carries. The drag pasteboard is one of those
//! AppKit lets any app read with no prompt (`NSPasteboard.AccessBehavior`).

use std::path::PathBuf;

use objc2::rc::Retained;
use objc2_app_kit::{
    NSFilePromiseReceiver, NSPasteboard, NSPasteboardItem, NSPasteboardNameDrag,
    NSPasteboardTypeFileURL, NSPasteboardTypeString,
};
use objc2_foundation::{NSString, NSURL};

/// The most text a drag's item is read for; longer text is left for the drop to fetch.
pub const TEXT_MAX: usize = 64 * 1024;

/// The type a file promise names its file's content type under.
const PROMISED_CONTENT_TYPE: &str = "com.apple.pasteboard.promised-file-content-type";

/// A file a drag names.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileFound {
    /// Its path, resolved.
    pub path: PathBuf,
    /// Its size in bytes; a folder's own entry size.
    pub size: u64,
    /// Whether it is a folder.
    pub folder: bool,
}

/// What one item of a drag holds, as far as the worker reads it before the drop.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Found {
    /// Every type the item offers.
    pub types: Vec<String>,
    /// The file its URL names, when it names one that exists.
    pub file: Option<FileFound>,
    /// A file promise's content type, when the item promises a file rather than naming one.
    pub promised: Option<String>,
    /// Its text, up to [`TEXT_MAX`] bytes.
    pub text: Option<String>,
    /// The types it carries as data, when it names and promises no file: every type but the
    /// bookkeeping: file promises', dynamic types and legacy spellings of a file's URL.
    pub carried: Vec<String>,
}

/// Types never carried as data: file promises' bookkeeping, the system's dynamic types and
/// legacy spellings of a file's URL, which the files already say.
pub(crate) fn bookkeeping(uti: &str, promises: &[String]) -> bool {
    uti.starts_with("dyn.")
        || uti.starts_with("com.apple.pasteboard.promised-")
        || uti.starts_with("NSFilenamesPboardType")
        || uti == "com.apple.NSFilePromiseItemMetaData"
        || uti == "public.file-url"
        || uti == "CorePasteboardFlavorType 0x6675726C"
        || promises.iter().any(|p| p == uti)
}

/// A watch on a pasteboard's change count, for a drag beginning.
#[derive(Debug)]
pub struct DragWatch {
    board: Retained<NSPasteboard>,
    marked: isize,
}

impl DragWatch {
    /// The system's drag pasteboard.
    #[must_use]
    pub fn drag() -> Self {
        // SAFETY: framework-provided constant string.
        Self::on(NSPasteboard::pasteboardWithName(unsafe { NSPasteboardNameDrag }))
    }

    /// The pasteboard called `name`: a test's own.
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self::on(NSPasteboard::pasteboardWithName(&NSString::from_str(name)))
    }

    fn on(board: Retained<NSPasteboard>) -> Self {
        let marked = board.changeCount();
        Self { board, marked }
    }

    /// Note the count now, at a press: only a change after this is a drag.
    pub fn mark(&mut self) {
        self.marked = self.board.changeCount();
    }

    /// Note `count` as the count at the press, read where the press was seen
    /// ([`change_count`]): only a change after it is a drag.
    pub const fn mark_at(&mut self, count: isize) {
        self.marked = count;
    }

    /// Whether the count moved since the mark.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.board.changeCount() != self.marked
    }

    /// What a drag that began since the mark carries, once: `None` until the count moves, and
    /// again after it is reported, until it moves again. A board with no items yet is a write
    /// under way: the clear moved the count and the write does not move it again, so the change
    /// stays unreported until the items are there.
    pub fn began(&mut self) -> Option<Vec<Found>> {
        let count = self.board.changeCount();
        if count == self.marked {
            return None;
        }
        let found = self.read();
        if found.is_empty() {
            return None;
        }
        self.marked = count;
        Some(found)
    }

    /// What the pasteboard's items hold now.
    #[must_use]
    pub fn read(&self) -> Vec<Found> {
        Self::read_board(&self.board)
    }

    /// What `board`'s items hold now.
    pub(crate) fn read_board(board: &NSPasteboard) -> Vec<Found> {
        let Some(items) = board.pasteboardItems() else { return Vec::new() };
        let promises: Vec<String> =
            NSFilePromiseReceiver::readableDraggedTypes().iter().map(|t| t.to_string()).collect();
        items.iter().map(|item| found(&item, &promises)).collect()
    }
}

/// The change count of the drag pasteboard, or of the one called `name` (a test's), now: read
/// where a press is seen, so a drag that begins before the watch next looks still counts.
#[must_use]
pub fn change_count(name: Option<&str>) -> isize {
    let board = match name {
        Some(name) => NSPasteboard::pasteboardWithName(&NSString::from_str(name)),
        // SAFETY: framework-provided constant string.
        None => NSPasteboard::pasteboardWithName(unsafe { NSPasteboardNameDrag }),
    };
    board.changeCount()
}

fn found(item: &NSPasteboardItem, promises: &[String]) -> Found {
    let types: Vec<String> = item.types().iter().map(|t| t.to_string()).collect();
    let has = |uti: &str| types.iter().any(|t| t == uti);
    // SAFETY: framework-provided constant strings.
    let (file_url, text) = unsafe { (NSPasteboardTypeFileURL, NSPasteboardTypeString) };
    let file = has(&file_url.to_string())
        .then(|| item.stringForType(file_url))
        .flatten()
        .and_then(|url| NSURL::URLWithString(&url))
        .and_then(|url| url.path())
        .and_then(|path| {
            let path = PathBuf::from(path.to_string());
            let meta = std::fs::metadata(&path).ok()?;
            let path = path.canonicalize().unwrap_or(path);
            Some(FileFound { path, size: meta.len(), folder: meta.is_dir() })
        });
    // A file URL's item offers `com.apple.pasteboard.promised-file-url` too, and is no promise.
    let promised = if file.is_some() {
        None
    } else if has(PROMISED_CONTENT_TYPE) {
        item.stringForType(&NSString::from_str(PROMISED_CONTENT_TYPE)).map(|s| s.to_string())
    } else {
        types.iter().any(|t| promises.contains(t)).then(String::new)
    };
    let text = if file.is_none() && has(&text.to_string()) {
        item.dataForType(text)
            .filter(|data| data.length() <= TEXT_MAX)
            .and_then(|data| String::from_utf8(data.to_vec()).ok())
    } else {
        None
    };
    let carried = if file.is_none() && promised.is_none() {
        types.iter().filter(|t| !bookkeeping(t, promises)).cloned().collect()
    } else {
        Vec::new()
    };
    Found { types, file, promised, text, carried }
}

#[cfg(test)]
mod tests {
    use objc2::msg_send;
    use objc2_app_kit::NSPasteboardItem;

    use super::*;

    /// A write moves the count and is reported once, not at the clear before it; a folder reads
    /// as a folder; text past the cap is left out, though its type is still carried; nothing is
    /// reported before a change.
    #[test]
    fn a_change_is_reported_once_with_what_the_items_hold() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("com.aislopware.slopty.test.dnd.watch.{}", std::process::id());
        let board = NSPasteboard::pasteboardWithName(&NSString::from_str(&name));
        let mut watch = DragWatch::named(&name);
        assert_eq!(watch.began(), None, "nothing changed");
        board.clearContents();
        assert!(watch.changed(), "the clear moves the count");
        assert_eq!(watch.began(), None, "a cleared board is a write under way");
        let folder = NSURL::fileURLWithPath(&NSString::from_str(&dir.path().to_string_lossy()));
        let long = NSPasteboardItem::new();
        // SAFETY: framework-provided constant string.
        let _set = long.setString_forType(&NSString::from_str(&"x".repeat(TEXT_MAX + 1)), unsafe {
            NSPasteboardTypeString
        });
        let wrote = board.writeObjects(&objc2_foundation::NSArray::from_retained_slice(&[
            objc2::runtime::ProtocolObject::from_retained(folder),
            objc2::runtime::ProtocolObject::from_retained(long),
        ]));
        assert!(wrote);
        assert!(watch.changed());
        let found = watch.began().expect("the write is a change");
        assert_eq!(found.len(), 2, "{found:?}");
        let folder = found[0].file.as_ref().expect("the folder's URL");
        assert!(folder.folder, "{folder:?}");
        assert_eq!(found[1].text, None, "past the cap: {:?}", found[1].types);
        assert!(found[0].carried.is_empty(), "a file carries no data: {:?}", found[0].types);
        assert!(found[1].carried.iter().any(|t| t == "public.utf8-plain-text"), "{found:?}");
        assert!(!found[1].carried.iter().any(|t| t.starts_with("dyn.")), "{found:?}");
        assert_eq!(watch.began(), None, "reported once");
        watch.mark();
        assert!(!watch.changed(), "marked at the current count");
        // SAFETY: AppKit rule: `releaseGlobally` takes no arguments; `board` is not used after.
        unsafe {
            let () = msg_send![&*board, releaseGlobally];
        }
    }
}

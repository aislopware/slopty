//! The worker's pasteboard, for clipboard sync with clients.
//!
//! [`Board`] is the clipboard seam, compiled on every target: the little clipboard sync needs
//! from a pasteboard. That is its `changeCount` (macOS has no change notification, so it is
//! polled), the types and bytes of what it holds, and a way to replace that. [`MacBoard`] is
//! `NSPasteboard`, either the general one or a named one; tests use
//! a named one ([`MacBoard::unique`]) and release it, so no test ever touches the user's
//! clipboard.

#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::runtime::ProtocolObject;
#[cfg(target_os = "macos")]
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardType, NSPasteboardTypeFileURL,
    NSPasteboardTypeHTML, NSPasteboardTypePNG, NSPasteboardTypeRTF, NSPasteboardTypeString,
    NSPasteboardTypeTIFF, NSPasteboardWriting,
};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSArray, NSData, NSString, NSURL};
pub use slopty_proto::transfer::{ClipFormat, ORIGIN_TYPE};
/// nspasteboard.org's marker for a secret (a password manager's copy); never synced.
pub const CONCEALED_TYPE: &str = "org.nspasteboard.ConcealedType";
/// nspasteboard.org's marker for contents that are about to go again; never synced.
pub const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

// The formats on the wire are platform-neutral ([`ClipFormat`]); a board speaks its own
// platform's type names, and this is where the two meet. On macOS the names come from
// AppKit's statics; elsewhere a board's types are MIME types.
#[cfg(target_os = "macos")]
/// The type `format` has on this platform's pasteboard: the UTI AppKit names it by.
#[must_use]
pub fn board_type(format: ClipFormat) -> String {
    // SAFETY: the `NSPasteboardType*` statics are AppKit constants, valid for the process
    // lifetime.
    let kind: &NSPasteboardType = unsafe {
        match format {
            ClipFormat::FileUrls => NSPasteboardTypeFileURL,
            ClipFormat::Png => NSPasteboardTypePNG,
            ClipFormat::Tiff => NSPasteboardTypeTIFF,
            ClipFormat::Rtf => NSPasteboardTypeRTF,
            ClipFormat::Html => NSPasteboardTypeHTML,
            ClipFormat::Text => NSPasteboardTypeString,
        }
    };
    kind.to_string()
}

#[cfg(not(target_os = "macos"))]
/// The type `format` has on this platform's pasteboard: its MIME type.
#[must_use]
pub fn board_type(format: ClipFormat) -> String {
    format.mime().to_owned()
}

/// The format a pasteboard type is, if clipboard sync carries it.
#[must_use]
pub fn format_of(board_type: &str) -> Option<ClipFormat> {
    ClipFormat::ALL.into_iter().find(|&format| self::board_type(format) == board_type)
}

/// One pasteboard item: its representations, as (type, bytes).
pub type Item = Vec<(String, Vec<u8>)>;

/// A pasteboard as clipboard sync sees it.
pub trait Board: Send + Sync {
    /// The count that moves on every change of owner.
    fn change_count(&self) -> isize;
    /// The types the first item holds.
    fn types(&self) -> Vec<String>;
    /// The first item's bytes of type `kind`.
    fn data(&self, kind: &str) -> Option<Vec<u8>>;
    /// The `public.file-url` of every item that has one, as a path URL: a file named by
    /// reference (`file:///.file/id=…`, as Finder copies) means nothing on another machine.
    fn file_urls(&self) -> Vec<String>;
    /// Replace the contents with `items`. The `changeCount` the write left, `None` when it
    /// failed.
    fn write(&self, items: &[Item]) -> Option<isize>;
}

/// The board of a worker that has no clipboard to sync yet.
///
/// That is Linux, where no Wayland or X11 board is wired up. It never changes and holds
/// nothing, so nothing is sent from it, and every write fails, so a paste into it is refused
/// rather than lost.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Unsupported;

impl Board for Unsupported {
    fn change_count(&self) -> isize {
        0
    }

    fn types(&self) -> Vec<String> {
        Vec::new()
    }

    fn data(&self, _kind: &str) -> Option<Vec<u8>> {
        None
    }

    fn file_urls(&self) -> Vec<String> {
        Vec::new()
    }

    fn write(&self, _items: &[Item]) -> Option<isize> {
        None
    }
}

/// Held across every `NSPasteboard` call in this process. AppKit documents no thread rule for it,
/// but Apple has told a developer it is not safe off one thread, and crash reports show its type
/// cache racing between two threads that use it at once. The worker polls from a blocking thread
/// while a paste writes from another, so they take turns here.
#[cfg(target_os = "macos")]
static ONE_AT_A_TIME: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// `NSPasteboard`: the general pasteboard, or a named one.
#[cfg(target_os = "macos")]
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MacBoard {
    /// `None` is the general pasteboard.
    name: Option<String>,
}

#[cfg(target_os = "macos")]
impl MacBoard {
    /// The general pasteboard: the one ⌘C and ⌘V use.
    #[must_use]
    pub const fn general() -> Self {
        Self { name: None }
    }

    /// The pasteboard called `name`, created on first use.
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self { name: Some(name.to_owned()) }
    }

    /// A pasteboard nobody else uses, for a test; [`MacBoard::release`] it after.
    #[must_use]
    pub fn unique() -> Self {
        Self { name: Some(NSPasteboard::pasteboardWithUniqueName().name().to_string()) }
    }

    /// Its name, `None` for the general pasteboard.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Let the pasteboard server drop a named pasteboard (a no-op for the general one).
    pub fn release(&self) {
        if self.name.is_none() {
            return;
        }
        let _turn = ONE_AT_A_TIME.lock();
        let board = self.board();
        // SAFETY: `-[NSPasteboard releaseGlobally]` takes no arguments and returns void
        // (`NSPasteboard.h`); objc2 does not bind it.
        unsafe {
            let () = objc2::msg_send![&*board, releaseGlobally];
        }
    }

    fn board(&self) -> Retained<NSPasteboard> {
        match &self.name {
            None => NSPasteboard::generalPasteboard(),
            Some(name) => NSPasteboard::pasteboardWithName(&NSString::from_str(name)),
        }
    }

    fn first_item(&self) -> Option<Retained<NSPasteboardItem>> {
        self.board().pasteboardItems()?.firstObject()
    }
}

#[cfg(target_os = "macos")]
impl Board for MacBoard {
    fn change_count(&self) -> isize {
        let _turn = ONE_AT_A_TIME.lock();
        self.board().changeCount()
    }

    fn types(&self) -> Vec<String> {
        let _turn = ONE_AT_A_TIME.lock();
        self.first_item()
            .map(|item| item.types().iter().map(|t| t.to_string()).collect())
            .unwrap_or_default()
    }

    fn data(&self, kind: &str) -> Option<Vec<u8>> {
        let _turn = ONE_AT_A_TIME.lock();
        // `dataForType:` copies the bytes out of the pasteboard server.
        Some(self.first_item()?.dataForType(&NSString::from_str(kind))?.to_vec())
    }

    fn file_urls(&self) -> Vec<String> {
        let _turn = ONE_AT_A_TIME.lock();
        let Some(items) = self.board().pasteboardItems() else { return Vec::new() };
        let kind = NSString::from_str(&board_type(ClipFormat::FileUrls));
        items
            .iter()
            .filter_map(|item| {
                let text = item.stringForType(&kind)?;
                let url = NSURL::URLWithString(&text)?;
                Some(url.filePathURL()?.absoluteString()?.to_string())
            })
            .collect()
    }

    fn write(&self, items: &[Item]) -> Option<isize> {
        let _turn = ONE_AT_A_TIME.lock();
        let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
            .iter()
            .map(|reps| {
                let item = NSPasteboardItem::new();
                for (kind, bytes) in reps {
                    // `setData:forType:` copies the data; a type the item refuses is left out.
                    let _set =
                        item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(kind));
                }
                ProtocolObject::from_retained(item)
            })
            .collect();
        let objects = NSArray::from_retained_slice(&objects);
        let board = self.board();
        // Another process that reads between the clear and the write sees an empty board under
        // the new count, so the items are built first to keep that gap short.
        let _previous = board.clearContents();
        board.writeObjects(&objects).then(|| board.changeCount())
    }
}

#[cfg(test)]
mod unsupported_tests {
    use super::{Board as _, ClipFormat, Unsupported, board_type};

    /// A board with no clipboard behind it offers nothing and refuses a paste.
    #[test]
    fn an_unsupported_board_holds_nothing_and_refuses_writes() {
        let board = Unsupported;
        let text = vec![(board_type(ClipFormat::Text), b"hi".to_vec())];
        assert_eq!(board.write(&[text]), None);
        assert_eq!(board.change_count(), 0);
        assert!(board.types().is_empty() && board.file_urls().is_empty());
        assert_eq!(board.data(&board_type(ClipFormat::Text)), None);
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    /// Readers and writers on several threads at once, as the worker's poller and a paste are,
    /// each see whole contents and nothing crashes.
    #[test]
    fn threads_take_turns_on_the_pasteboard() {
        let board = MacBoard::unique();
        let text = board_type(ClipFormat::Text);
        std::thread::scope(|scope| {
            for n in 0..4_u8 {
                let (board, text) = (&board, &text);
                scope.spawn(move || {
                    for _ in 0..100 {
                        let mine = vec![n; 64];
                        board.write(&[vec![(text.clone(), mine)]]);
                        let _count = board.change_count();
                        let _types = board.types();
                        if let Some(bytes) = board.data(text) {
                            assert!(bytes.len() == 64 && bytes.iter().all(|b| *b == bytes[0]));
                        }
                    }
                });
            }
        });
        board.release();
    }

    /// Each format is a UTI on a Mac's pasteboard, and each of those UTIs is that format again;
    /// a type clipboard sync does not carry is no format.
    #[test]
    fn each_format_is_an_apple_uti_and_back() {
        let utis = [
            (ClipFormat::FileUrls, "public.file-url"),
            (ClipFormat::Png, "public.png"),
            (ClipFormat::Tiff, "public.tiff"),
            (ClipFormat::Rtf, "public.rtf"),
            (ClipFormat::Html, "public.html"),
            (ClipFormat::Text, "public.utf8-plain-text"),
        ];
        for (format, uti) in utis {
            assert_eq!(board_type(format), uti);
            assert_eq!(format_of(uti), Some(format));
        }
        assert_eq!(format_of("com.adobe.pdf"), None);
        assert_eq!(format_of(ORIGIN_TYPE), None);
    }

    /// A named pasteboard takes items, says what it holds, moves its count on a write, and
    /// is released after; the general pasteboard is never touched.
    #[test]
    fn a_named_board_round_trips_items() {
        let board = MacBoard::unique();
        let before = board.change_count();
        let text = board_type(ClipFormat::Text);
        let url = board_type(ClipFormat::FileUrls);
        let items = vec![
            vec![(url.clone(), b"file:///tmp/a".to_vec()), (ORIGIN_TYPE.to_owned(), b"o".to_vec())],
            vec![(url, b"file:///tmp/b".to_vec())],
        ];
        let after = board.write(&items).unwrap();
        assert_ne!(after, before);
        assert_eq!(board.change_count(), after);
        assert!(board.types().iter().any(|t| t == ORIGIN_TYPE), "{:?}", board.types());
        assert_eq!(board.file_urls(), ["file:///tmp/a", "file:///tmp/b"]);
        assert_eq!(board.data(ORIGIN_TYPE).unwrap(), b"o");

        board.write(&[vec![(text.clone(), "héllo".as_bytes().to_vec())]]).unwrap();
        assert_eq!(board.data(&text).unwrap(), "héllo".as_bytes());
        assert!(board.file_urls().is_empty());
        board.release();
    }
}

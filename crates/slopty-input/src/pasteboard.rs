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
use objc2_foundation::{NSArray, NSData, NSString};
pub use slopty_proto::transfer::ORIGIN_TYPE;
/// nspasteboard.org's marker for a secret (a password manager's copy); never synced.
pub const CONCEALED_TYPE: &str = "org.nspasteboard.ConcealedType";
/// nspasteboard.org's marker for contents that are about to go again; never synced.
pub const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

/// The representations clipboard sync carries, richest first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rep {
    /// `public.file-url`, one per item.
    FileUrl,
    /// `public.png`.
    Png,
    /// `public.tiff`, what a Mac screenshot is on the pasteboard.
    Tiff,
    /// `public.rtf`.
    Rtf,
    /// `public.html`.
    Html,
    /// `public.utf8-plain-text`.
    Text,
}

impl Rep {
    /// Every representation, richest first.
    pub const ALL: [Self; 6] =
        [Self::FileUrl, Self::Png, Self::Tiff, Self::Rtf, Self::Html, Self::Text];
}

// The type names on the wire are Apple's UTIs; on macOS they come from AppKit's statics. Another
// platform names them in its own module.
#[cfg(target_os = "macos")]
impl Rep {
    /// The uniform type identifier, as AppKit spells it.
    #[must_use]
    pub fn uti(self) -> String {
        // SAFETY: the `NSPasteboardType*` statics are AppKit constants, valid for the process
        // lifetime.
        let kind: &NSPasteboardType = unsafe {
            match self {
                Self::FileUrl => NSPasteboardTypeFileURL,
                Self::Png => NSPasteboardTypePNG,
                Self::Tiff => NSPasteboardTypeTIFF,
                Self::Rtf => NSPasteboardTypeRTF,
                Self::Html => NSPasteboardTypeHTML,
                Self::Text => NSPasteboardTypeString,
            }
        };
        kind.to_string()
    }

    /// The representation `uti` names, if clipboard sync carries it.
    #[must_use]
    pub fn of(uti: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rep| rep.uti() == uti)
    }
}

/// One pasteboard item: its representations, as (type, bytes).
pub type Item = Vec<(String, Vec<u8>)>;

/// A pasteboard as clipboard sync sees it.
pub trait Board: Send + Sync {
    /// The count that moves on every change of owner.
    fn change_count(&self) -> isize;
    /// The types the first item holds.
    fn types(&self) -> Vec<String>;
    /// The first item's bytes of type `uti`.
    fn data(&self, uti: &str) -> Option<Vec<u8>>;
    /// The `public.file-url` of every item that has one.
    fn file_urls(&self) -> Vec<String>;
    /// Replace the contents with `items`. The `changeCount` the write left, `None` when it
    /// failed.
    fn write(&self, items: &[Item]) -> Option<isize>;
}

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
        self.board().changeCount()
    }

    fn types(&self) -> Vec<String> {
        self.first_item()
            .map(|item| item.types().iter().map(|t| t.to_string()).collect())
            .unwrap_or_default()
    }

    fn data(&self, uti: &str) -> Option<Vec<u8>> {
        // `dataForType:` copies the bytes out of the pasteboard server.
        Some(self.first_item()?.dataForType(&NSString::from_str(uti))?.to_vec())
    }

    fn file_urls(&self) -> Vec<String> {
        let Some(items) = self.board().pasteboardItems() else { return Vec::new() };
        let kind = Rep::FileUrl.uti();
        let kind = NSString::from_str(&kind);
        items.iter().filter_map(|item| Some(item.stringForType(&kind)?.to_string())).collect()
    }

    fn write(&self, items: &[Item]) -> Option<isize> {
        let board = self.board();
        let _previous = board.clearContents();
        let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
            .iter()
            .map(|reps| {
                let item = NSPasteboardItem::new();
                for (uti, bytes) in reps {
                    // `setData:forType:` copies the data; a type the item refuses is left out.
                    let _set =
                        item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(uti));
                }
                ProtocolObject::from_retained(item)
            })
            .collect();
        board.writeObjects(&NSArray::from_retained_slice(&objects)).then(|| board.changeCount())
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    #[test]
    fn the_reps_are_apples_utis() {
        assert_eq!(Rep::Text.uti(), "public.utf8-plain-text");
        assert_eq!(Rep::FileUrl.uti(), "public.file-url");
        assert_eq!(Rep::of("public.png"), Some(Rep::Png));
        assert_eq!(Rep::of("com.adobe.pdf"), None);
    }

    /// A named pasteboard takes items, says what it holds, moves its count on a write, and
    /// is released after; the general pasteboard is never touched.
    #[test]
    fn a_named_board_round_trips_items() {
        let board = MacBoard::unique();
        let before = board.change_count();
        let text = Rep::Text.uti();
        let url = Rep::FileUrl.uti();
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

//! What the helper's drag carries into a target: one pasteboard item per thing the client drags.
//!
//! A file already whole on the worker is its file URL. Anything else is a promise the pasteboard
//! keeps until the target reads it: data the worker fetches from the client, or a file still
//! arriving, whose `public.file-url` is answered once it is whole. A target reads at the drop,
//! or half a second after it (P0 (8a)), so a promise answered then lands at the point as a
//! whole file would.

use std::path::PathBuf;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread as _, DefinedClass as _, define_class, msg_send};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardItemDataProvider, NSPasteboardType,
    NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSObject, NSObjectProtocol, NSString, NSURL};

/// One thing a drag carries.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Item {
    /// A file whole on this Mac, named by its URL.
    File(PathBuf),
    /// Representations, by uniform type identifier, given only when a target reads them
    /// ([`Provide`]). `public.file-url` among them names a file that is whole by then.
    Later(Vec<String>),
}

/// Answers a promised representation.
///
/// It gets the item's index in the drag and the type asked for, and gives the bytes, or `None`
/// when there are none to give. AppKit asks on the main thread while the
/// target reads, and the bytes must be there when this returns, so it may wait for them, for a
/// bounded time.
pub type Provide = Arc<dyn Fn(usize, &str) -> Option<Vec<u8>> + Send + Sync>;

/// The `public.file-url` bytes naming `path`, as a promise answers them.
#[must_use]
pub fn file_url_bytes(path: &std::path::Path) -> Vec<u8> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    url.absoluteString().map(|s| s.to_string().into_bytes()).unwrap_or_default()
}

struct Ivars {
    item: usize,
    provide: Provide,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Promise` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "SloptyDragPromise"]
    #[ivars = Ivars]
    struct Promise;

    unsafe impl NSObjectProtocol for Promise {}

    unsafe impl NSPasteboardItemDataProvider for Promise {
        #[unsafe(method(pasteboard:item:provideDataForType:))]
        fn provide_data(
            &self,
            _pasteboard: Option<&NSPasteboard>,
            item: &NSPasteboardItem,
            kind: &NSPasteboardType,
        ) {
            let ivars = self.ivars();
            if let Some(bytes) = (ivars.provide)(ivars.item, &kind.to_string()) {
                let _set = item.setData_forType(&NSData::with_bytes(&bytes), kind);
            }
        }
    }
);

impl Promise {
    fn new(item: usize, provide: Provide) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { item, provide });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with its ivars set.
        unsafe { msg_send![super(this), init] }
    }
}

impl std::fmt::Debug for Promise {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Promise").field("item", &self.ivars().item).finish_non_exhaustive()
    }
}

/// A drag's items as the pasteboard takes them, and the promises they hold, which must live as
/// long as a target may read them: until the next drag.
#[derive(Debug)]
pub struct Writers {
    writers: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>>,
    promises: Vec<Retained<Promise>>,
}

impl Writers {
    /// `items`, their promises answered by `provide`.
    #[must_use]
    pub fn new(items: &[Item], provide: &Provide) -> Self {
        let mut promises = Vec::new();
        let writers = items
            .iter()
            .enumerate()
            .map(|(n, item)| -> Retained<ProtocolObject<dyn NSPasteboardWriting>> {
                match item {
                    Item::File(path) => ProtocolObject::from_retained(NSURL::fileURLWithPath(
                        &NSString::from_str(&path.to_string_lossy()),
                    )),
                    Item::Later(types) => {
                        let item = NSPasteboardItem::new();
                        let promise = Promise::new(n, Arc::clone(provide));
                        let types: Vec<Retained<NSString>> =
                            types.iter().map(|t| NSString::from_str(t)).collect();
                        let _kept = item.setDataProvider_forTypes(
                            ProtocolObject::from_ref(&*promise),
                            &NSArray::from_retained_slice(&types),
                        );
                        promises.push(promise);
                        ProtocolObject::from_retained(item)
                    }
                }
            })
            .collect();
        Self { writers, promises }
    }

    /// The pasteboard writers, one per item, in order.
    #[must_use]
    pub fn writers(&self) -> &[Retained<ProtocolObject<dyn NSPasteboardWriting>>] {
        &self.writers
    }

    /// Put the items on `board` as a copy would: how a test reads them back without a drag.
    pub fn write(&self, board: &NSPasteboard) -> bool {
        board.clearContents();
        board.writeObjects(&NSArray::from_retained_slice(&self.writers))
    }

    /// How many promises the items hold.
    #[must_use]
    pub const fn promises(&self) -> usize {
        self.promises.len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::watch::DragWatch;

    /// Every item a drag carries reads back from a pasteboard as it was sent: a whole file by
    /// its URL, promised text and a promised file's URL answered only when read, each promise
    /// asked once. A named pasteboard of the test's own; the drag pasteboard is never touched.
    #[test]
    fn every_drag_item_reads_back_as_it_was_sent() {
        let dir = tempfile::tempdir().unwrap();
        let whole = dir.path().join("whole.txt");
        let later = dir.path().join("later.bin");
        std::fs::write(&whole, b"whole").unwrap();
        let name = format!("com.aislopware.slopty.test.dnd.items.{}", std::process::id());
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        let promised = later.clone();
        let provide: Provide = Arc::new(move |item, uti| {
            counter.fetch_add(1, Ordering::SeqCst);
            match (item, uti) {
                (1, "public.utf8-plain-text") => Some(b"dropped words".to_vec()),
                (2, "public.file-url") => {
                    // The upload finishes as the target asks.
                    std::fs::write(&promised, vec![7; 4096]).ok()?;
                    Some(file_url_bytes(&promised))
                }
                _ => None,
            }
        });
        let items = [
            Item::File(whole.clone()),
            Item::Later(vec!["public.utf8-plain-text".to_owned()]),
            Item::Later(vec!["public.file-url".to_owned()]),
        ];
        let writers = Writers::new(&items, &provide);
        assert_eq!(writers.promises(), 2);
        let board = NSPasteboard::pasteboardWithName(&NSString::from_str(&name));
        assert!(writers.write(&board), "the pasteboard took the items");
        assert_eq!(asked.load(Ordering::SeqCst), 0, "nothing is asked before a read");
        let watch = DragWatch::named(&name);
        let found = watch.read();
        assert_eq!(found.len(), 3, "{found:?}");
        let file = |n: usize| found[n].file.as_ref().map(|f| (f.path.clone(), f.size, f.folder));
        assert_eq!(file(0), Some((whole.canonicalize().unwrap(), 5, false)), "{found:?}");
        assert_eq!(found[1].text.as_deref(), Some("dropped words"), "{found:?}");
        assert_eq!(file(2), Some((later.canonicalize().unwrap(), 4096, false)), "{found:?}");
        assert_eq!(asked.load(Ordering::SeqCst), 2, "each promise asked once, when read");
        // SAFETY: AppKit rule: `releaseGlobally` takes no arguments; `board` is not used after.
        unsafe {
            let () = msg_send![&*board, releaseGlobally];
        }
    }
}

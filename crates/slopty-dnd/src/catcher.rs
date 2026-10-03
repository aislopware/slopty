//! Catching a drag out of an app on the worker, once the client's pointer has left the tile.
//!
//! The client begins its own drag and the worker lets the remote one go onto a window of the
//! helper's at the pointer: transparent, registered for files, promises and data, and answering
//! Copy, so the app it came from sees an ordinary copy and moves nothing. What it takes: the
//! file URLs as references (the files stay where they are), each promised file called into a
//! folder of the drag's own at the drop (the one moment macOS 27 allows), and every other
//! representation whole up to a cap.

use std::path::{Path, PathBuf};

use objc2::ClassType as _;
use objc2::rc::Retained;
use objc2::runtime::AnyClass;
use objc2_app_kit::{NSFilePromiseReceiver, NSPasteboard, NSPasteboardItem};
use objc2_foundation::NSArray;

use crate::watch::{DragWatch, bookkeeping};

/// What a catcher took at the drop, before any promised file is written.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Caught {
    /// The files the drag named, left where they are.
    pub files: Vec<PathBuf>,
    /// Every other representation of the items without a file, whole: `(item, type, bytes)`.
    pub data: Vec<(usize, String, Vec<u8>)>,
    /// Representations past the cap, not kept: `(item, type, size)`.
    pub too_big: Vec<(usize, String, u64)>,
    /// Promised files being called in; each ends in one [`Promised`].
    pub promises: usize,
}

/// A promised file called into the catcher's folder, or why it is not there.
pub type Promised = Result<PathBuf, String>;

/// What a drop on `board` carries, taken as [`Caught`] says, and the promise of each item that
/// promises a file and names none, in item order.
///
/// `NSPasteboard.readObjectsForClasses` gives one receiver per item that can make one, in item
/// order, so the `k`-th receiver belongs to the `k`-th such item; an item that names a file as
/// well keeps its file and leaves its receiver unused.
pub fn take(board: &NSPasteboard, max: u64) -> (Caught, Vec<Retained<NSFilePromiseReceiver>>) {
    let promise_types: Vec<String> =
        NSFilePromiseReceiver::readableDraggedTypes().iter().map(|t| t.to_string()).collect();
    let items: Vec<Retained<NSPasteboardItem>> =
        board.pasteboardItems().map(|items| items.to_vec()).unwrap_or_default();
    let receivers: Vec<Retained<NSFilePromiseReceiver>> = {
        let class: &AnyClass = NSFilePromiseReceiver::class();
        let classes = NSArray::from_slice(&[class]);
        // SAFETY: AppKit rule: `readObjectsForClasses:options:` returns instances of the classes
        // asked for, here only `NSFilePromiseReceiver`, so the cast of each holds.
        unsafe { board.readObjectsForClasses_options(&classes, None) }
            .map(|objects| {
                objects
                    .iter()
                    .filter_map(|object| object.downcast::<NSFilePromiseReceiver>().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    let found = DragWatch::read_board(board);
    let mut caught = Caught::default();
    let mut receivers = receivers.into_iter();
    let mut called = Vec::new();
    for (n, (item, found)) in items.iter().zip(&found).enumerate() {
        let promises =
            found.types.iter().any(|t| promise_types.contains(t)).then(|| receivers.next());
        if let Some(file) = &found.file {
            caught.files.push(file.path.clone());
            continue;
        }
        if let Some(Some(receiver)) = promises {
            called.push(receiver);
            continue;
        }
        for uti in found.types.iter().filter(|t| !bookkeeping(t, &promise_types)) {
            let Some(data) = item.dataForType(&objc2_foundation::NSString::from_str(uti)) else {
                continue;
            };
            let size = u64::try_from(data.length()).unwrap_or(u64::MAX);
            if size > max {
                caught.too_big.push((n, uti.clone(), size));
            } else {
                caught.data.push((n, uti.clone(), data.to_vec()));
            }
        }
    }
    caught.promises = called.len();
    (caught, called)
}

/// The folder a drag's promised files are called into: `<root>/<drag>`, made if missing.
pub fn landing(root: &Path, drag: &str) -> std::io::Result<PathBuf> {
    let dir = root.join(drag);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use objc2::msg_send;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::NSPasteboardItem;
    use objc2_foundation::{NSData, NSString, NSURL};

    use super::*;

    /// A drop's files are taken as references and moved nowhere, its text and a picture whole,
    /// a representation past the cap listed and not kept, and a file's own text (its path) never
    /// kept as data beside it.
    #[test]
    fn the_catcher_takes_urls_as_references_and_data_whole() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("kept in place.txt");
        std::fs::write(&file, b"stays").unwrap();
        let name = format!("com.aislopware.slopty.test.dnd.catch.{}", std::process::id());
        let board = NSPasteboard::pasteboardWithName(&NSString::from_str(&name));
        board.clearContents();
        let text = NSPasteboardItem::new();
        let _set = text.setData_forType(
            &NSData::with_bytes(b"caught words"),
            &NSString::from_str("public.utf8-plain-text"),
        );
        let _set = text.setData_forType(
            &NSData::with_bytes(&[0x89, b'P', b'N', b'G']),
            &NSString::from_str("public.png"),
        );
        let _set = text
            .setData_forType(&NSData::with_bytes(&[0; 64]), &NSString::from_str("com.adobe.pdf"));
        let url = NSURL::fileURLWithPath(&NSString::from_str(&file.to_string_lossy()));
        let wrote = board.writeObjects(&NSArray::from_retained_slice(&[
            ProtocolObject::from_retained(url),
            ProtocolObject::from_retained(text),
        ]));
        assert!(wrote);
        let (caught, promises) = take(&board, 16);
        assert_eq!(promises, []);
        assert_eq!(caught.files, vec![file.canonicalize().unwrap()], "{caught:?}");
        assert!(std::fs::read(&file).unwrap() == b"stays", "the file is where it was");
        let kept: Vec<(usize, &str, &[u8])> =
            caught.data.iter().map(|(n, t, b)| (*n, t.as_str(), b.as_slice())).collect();
        assert!(kept.contains(&(1, "public.utf8-plain-text", b"caught words")), "{kept:?}");
        assert!(kept.contains(&(1, "public.png", &[0x89, b'P', b'N', b'G'])), "{kept:?}");
        assert!(kept.iter().all(|(n, ..)| *n == 1), "nothing kept of the file's item: {kept:?}");
        assert_eq!(caught.too_big, vec![(1, "com.adobe.pdf".to_owned(), 64)]);
        assert_eq!(caught.promises, 0);
        let landed = landing(dir.path(), "drag-1").unwrap();
        assert!(landed.is_dir() && landed.ends_with("drag-1"));
        // SAFETY: AppKit rule: `releaseGlobally` takes no arguments; `board` is not used after.
        unsafe {
            let () = msg_send![&*board, releaseGlobally];
        }
    }
}

pub use view::{Catcher, CatcherEvents, SIDE};

mod view {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::ptr::NonNull;
    use std::sync::Arc;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2::{DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSDragOperation, NSDraggingDestination, NSDraggingInfo, NSFilePromiseReceiver, NSResponder,
        NSView, NSWindow,
    };
    use objc2_foundation::{
        NSArray, NSDictionary, NSError, NSObject, NSOperationQueue, NSPoint, NSRect, NSSize,
        NSString, NSURL,
    };

    use super::{Caught, Promised, take};
    use crate::window;

    /// The catcher window's side, in points: room for the two drags the worker posts over it.
    pub const SIDE: f64 = 64.0;

    /// What the catcher tells the helper. `promised` comes on an operation queue's thread.
    pub trait CatcherEvents: Send + Sync {
        /// The drag reached the catcher.
        fn entered(&self);
        /// The drop, as taken; its promises follow one [`Promised`] each.
        fn caught(&self, caught: Caught);
        /// A promised file written into the drag's folder, or why not.
        fn promised(&self, promised: Promised);
    }

    struct Ivars {
        /// Where this drag's promised files go.
        dir: RefCell<PathBuf>,
        /// The most bytes a representation is kept whole at.
        max: u64,
        /// Whether `entered` was said for this drag.
        entered: Cell<bool>,
        events: Arc<dyn CatcherEvents>,
        queue: Retained<NSOperationQueue>,
    }

    define_class!(
        // SAFETY:
        // - `NSView` may be subclassed; the overrides keep its contracts (a dragging destination).
        // - `CatcherView` does not implement `Drop`.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SloptyDragCatcherView"]
        #[ivars = Ivars]
        struct CatcherView;

        unsafe impl NSObjectProtocol for CatcherView {}

        unsafe impl NSDraggingDestination for CatcherView {
            #[unsafe(method(draggingEntered:))]
            fn dragging_entered(
                &self,
                _info: &ProtocolObject<dyn NSDraggingInfo>,
            ) -> NSDragOperation {
                if !self.ivars().entered.replace(true) {
                    self.ivars().events.entered();
                }
                NSDragOperation::Copy
            }

            #[unsafe(method(draggingUpdated:))]
            fn dragging_updated(
                &self,
                _info: &ProtocolObject<dyn NSDraggingInfo>,
            ) -> NSDragOperation {
                NSDragOperation::Copy
            }

            #[unsafe(method(prepareForDragOperation:))]
            fn prepare(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
                true
            }

            #[unsafe(method(performDragOperation:))]
            fn perform(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
                self.take(info);
                true
            }
        }
    );

    impl CatcherView {
        fn new(mtm: MainThreadMarker, max: u64, events: Arc<dyn CatcherEvents>) -> Retained<Self> {
            let ivars = Ivars {
                dir: RefCell::new(std::env::temp_dir()),
                max,
                entered: Cell::new(false),
                events,
                queue: NSOperationQueue::new(),
            };
            let this = Self::alloc(mtm).set_ivars(ivars);
            let frame = NSRect {
                origin: NSPoint { x: 0.0, y: 0.0 },
                size: NSSize { width: SIDE, height: SIDE },
            };
            // SAFETY: `NSView`'s designated initialiser on a freshly allocated instance.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        /// Take the drop, and call its promises in: inside `performDragOperation:`, the one
        /// moment macOS 27 lets a destination receive them.
        fn take(&self, info: &ProtocolObject<dyn NSDraggingInfo>) {
            let ivars = self.ivars();
            let (caught, receivers) = take(&info.draggingPasteboard(), ivars.max);
            ivars.events.caught(caught);
            let dir = NSURL::fileURLWithPath_isDirectory(
                &NSString::from_str(&ivars.dir.borrow().to_string_lossy()),
                true,
            );
            let options = NSDictionary::new();
            for receiver in receivers {
                let events = Arc::clone(&ivars.events);
                let reader = RcBlock::new(move |url: NonNull<NSURL>, error: *mut NSError| {
                    // SAFETY: AppKit rule: the reader gets a valid URL for the call, and an
                    // error that is null or valid for it.
                    let url = unsafe { url.as_ref() };
                    // SAFETY: as above.
                    let error = unsafe { error.as_ref() };
                    events.promised(match error {
                        Some(error) => Err(error.localizedDescription().to_string()),
                        None => url
                            .path()
                            .map(|p| PathBuf::from(p.to_string()))
                            .ok_or_else(|| "a promise answered no path".to_owned()),
                    });
                });
                // SAFETY: AppKit rule: called inside `performDragOperation:`; the options are an
                // empty dictionary, and the reader runs on `queue`, which lives in the view, so
                // it only touches `events`, which is `Send + Sync`.
                unsafe {
                    receiver.receivePromisedFilesAtDestination_options_operationQueue_reader(
                        &dir,
                        &options,
                        &ivars.queue,
                        &reader,
                    );
                }
            }
        }
    }

    impl std::fmt::Debug for CatcherView {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("CatcherView")
        }
    }

    /// The helper's catcher: a window of [`SIDE`] points, out of sight until placed.
    #[derive(Debug)]
    pub struct Catcher {
        window: Retained<NSWindow>,
        view: Retained<CatcherView>,
    }

    impl Catcher {
        /// A catcher that keeps representations whole up to `max` bytes and tells `events`.
        #[must_use]
        pub fn new(mtm: MainThreadMarker, max: u64, events: Arc<dyn CatcherEvents>) -> Self {
            let window = window::square(mtm, SIDE);
            let view = CatcherView::new(mtm, max, events);
            // A destination matches a drag by exact type, not by conformance, so every common
            // concrete type is named.
            let mut types: Vec<Retained<NSString>> = [
                "public.file-url",
                "public.url",
                "public.url-name",
                "public.utf8-plain-text",
                "public.utf16-plain-text",
                "public.plain-text",
                "public.rtf",
                "com.apple.flat-rtfd",
                "public.html",
                "public.png",
                "public.jpeg",
                "public.heic",
                "com.compuserve.gif",
                "public.tiff",
                "com.adobe.pdf",
                "public.vcard",
                "com.apple.webarchive",
            ]
            .iter()
            .map(|t| NSString::from_str(t))
            .collect();
            types.extend(NSFilePromiseReceiver::readableDraggedTypes().to_vec());
            view.registerForDraggedTypes(&NSArray::from_retained_slice(&types));
            window.setContentView(Some(&view));
            Self { window, view }
        }

        /// Wait under the global point `at` for a drag, calling its promises into `dir`.
        pub fn at(&self, at: (f64, f64), dir: PathBuf) {
            self.view.ivars().dir.replace(dir);
            self.view.ivars().entered.set(false);
            window::place(&self.window, at, SIDE);
        }

        /// Its window's number (`CGWindowID`).
        #[must_use]
        pub fn window_number(&self) -> isize {
            self.window.windowNumber()
        }

        /// Out of sight.
        pub fn stop(&self) {
            self.window.orderOut(None);
        }
    }
}

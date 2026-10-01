//! Dragging a worker's file out of the app (macOS): a file promise per file, and data items.
//!
//! Finder, Mail or any app that takes files receives an `NSFilePromiseProvider`: the file does
//! not exist here until the drop, when the receiver names where it goes and the promise is kept
//! by bringing the file down from the worker. That runs on an operation queue of its own, never
//! the main thread, so a large file does not stall the UI. Data (a drag out of a worker's app
//! of text or a picture) goes as an `NSPasteboardItem` whose types are declared up front and
//! whose bytes are asked for only when the receiver reads them ([`Data`]).
//!
//! A drag can carry a tag of the app's own, so a drop back onto this app's window knows the
//! drag for one it began (`DropSink::over`, in [`crate::file_drop`]).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{
    AllocAnyThread as _, DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class,
    msg_send,
};
use objc2_app_kit::{
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingInfo, NSDraggingItem,
    NSDraggingSession, NSDraggingSource, NSFilePromiseProvider, NSFilePromiseProviderDelegate,
    NSPasteboard, NSPasteboardItem, NSPasteboardItemDataProvider, NSPasteboardType,
    NSPasteboardWriting, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSData, NSError, NSObject, NSOperationQueue, NSPoint, NSRect, NSSize, NSString, NSURL,
};

/// Makes the promised file exist at exactly this path; the error is for a person.
pub type Keep = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// One file promised to a drop.
#[derive(Clone)]
pub struct Promise {
    /// The file's name, as the receiver will first try it.
    pub name: String,
    /// Keeps the promise.
    pub keep: Keep,
}

impl std::fmt::Debug for Promise {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Promise").field("name", &self.name).finish_non_exhaustive()
    }
}

/// Gives the bytes of one type of a data item when the receiver reads it, on the main thread;
/// `None` when they are not there.
pub type Give = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// One data item of a drag: the types it offers, richest first, and what gives their bytes.
#[derive(Clone)]
pub struct Data {
    /// Uniform type identifiers.
    pub types: Vec<String>,
    /// Gives each type's bytes.
    pub give: Give,
}

impl std::fmt::Debug for Data {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Data").field("types", &self.types).finish_non_exhaustive()
    }
}

/// Drags kept alive after they start: a promise provider holds its delegate weakly, and the
/// write can come long after the drop. The oldest go once there are this many.
const LIVE_DRAGS: usize = 16;

/// A drag begun: its source, the delegates of its promises and the givers of its data.
type Live = (Retained<Source>, Vec<Retained<Keeper>>, Vec<Retained<Giver>>);

thread_local! {
    static LIVE: RefCell<VecDeque<Live>> = RefCell::default();
}

struct KeeperIvars {
    name: String,
    keep: Keep,
    queue: Retained<NSOperationQueue>,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Keeper` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "SloptyFilePromiseKeeper"]
    #[ivars = KeeperIvars]
    struct Keeper;

    unsafe impl NSObjectProtocol for Keeper {}

    unsafe impl NSFilePromiseProviderDelegate for Keeper {
        #[unsafe(method_id(filePromiseProvider:fileNameForType:))]
        fn file_name(
            &self,
            _provider: &NSFilePromiseProvider,
            _file_type: &NSString,
        ) -> Retained<NSString> {
            NSString::from_str(&self.ivars().name)
        }

        #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
        fn write_promise(
            &self,
            _provider: &NSFilePromiseProvider,
            url: &NSURL,
            done: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let kept = url
                .path()
                .map(|p| PathBuf::from(p.to_string()))
                .ok_or_else(|| "not a file URL".to_owned())
                .and_then(|path| (self.ivars().keep)(&path));
            match kept {
                Ok(()) => done.call((std::ptr::null_mut(),)),
                Err(why) => {
                    tracing::warn!(name = %self.ivars().name, %why, "file promise broken");
                    let domain = NSString::from_str("com.aislopware.slopty");
                    // SAFETY: Foundation rule: any domain string, any code, and a nil user
                    // info make a valid error.
                    let error = unsafe { NSError::errorWithDomain_code_userInfo(&domain, 1, None) };
                    done.call((Retained::as_ptr(&error).cast_mut(),));
                }
            }
        }

        #[unsafe(method_id(operationQueueForFilePromiseProvider:))]
        fn queue(&self, _provider: &NSFilePromiseProvider) -> Retained<NSOperationQueue> {
            Retained::clone(&self.ivars().queue)
        }
    }
);

impl Keeper {
    fn new(promise: Promise) -> Retained<Self> {
        let Promise { name, keep } = promise;
        let this =
            Self::alloc().set_ivars(KeeperIvars { name, keep, queue: NSOperationQueue::new() });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Giver` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "SloptyDragDataGiver"]
    #[ivars = Give]
    struct Giver;

    unsafe impl NSObjectProtocol for Giver {}

    unsafe impl NSPasteboardItemDataProvider for Giver {
        #[unsafe(method(pasteboard:item:provideDataForType:))]
        fn provide_data(
            &self,
            _pasteboard: Option<&NSPasteboard>,
            item: &NSPasteboardItem,
            kind: &NSPasteboardType,
        ) {
            let uti = kind.to_string();
            let Some(bytes) = (self.ivars())(&uti) else {
                tracing::debug!(uti, "drag data not there");
                return;
            };
            let set = item.setData_forType(&NSData::with_bytes(&bytes), kind);
            tracing::debug!(uti, bytes = bytes.len(), set, "drag data given");
        }
    }
);

impl Giver {
    fn new(give: Give) -> Retained<Self> {
        let this = Self::alloc().set_ivars(give);
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Source` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyDragSource"]
    #[ivars = Option<u64>]
    struct Source;

    unsafe impl NSObjectProtocol for Source {}

    unsafe impl NSDraggingSource for Source {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operations(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }
    }
);

impl Source {
    fn new(mtm: MainThreadMarker, tag: Option<u64>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(tag);
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// Start dragging `files` out of the window, from the mouse event being handled.
///
/// That event is a press or a drag of the left button. Returns whether a drag began: `false`
/// off the main thread, with no mouse event, or with nothing to drag.
pub fn drag_out(files: Vec<Promise>) -> bool {
    drag_out_items(files, Vec::new(), None)
}

/// Start dragging `files` and `data` out of the window, from the mouse event being handled, as
/// [`drag_out`] does, tagged `tag` for a drop back onto this app
/// ([`crate::file_drop::DropSink::over`]).
pub fn drag_out_items(files: Vec<Promise>, data: Vec<Data>, tag: Option<u64>) -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    if files.is_empty() && data.is_empty() {
        return false;
    }
    let app = NSApplication::sharedApplication(mtm);
    let Some(event) = app.currentEvent() else {
        tracing::debug!("drag out: no event");
        return false;
    };
    let Some(view) = event.window(mtm).or_else(|| app.keyWindow()).and_then(|w| w.contentView())
    else {
        tracing::debug!("drag out: no view");
        return false;
    };
    let at = event.locationInWindow();
    let mut keepers = Vec::with_capacity(files.len());
    let mut items = Vec::with_capacity(files.len());
    for (n, promise) in files.into_iter().enumerate() {
        let extension =
            Path::new(&promise.name).extension().and_then(|e| e.to_str()).unwrap_or("").to_owned();
        let keeper = Keeper::new(promise);
        let provider = NSFilePromiseProvider::initWithFileType_delegate(
            NSFilePromiseProvider::alloc(),
            &NSString::from_str("public.data"),
            ProtocolObject::from_ref(&*keeper),
        );
        let writer: &ProtocolObject<dyn NSPasteboardWriting> = ProtocolObject::from_ref(&*provider);
        let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
        #[expect(deprecated, reason = "`iconForContentType:` needs UniformTypeIdentifiers")]
        let icon = NSWorkspace::sharedWorkspace().iconForFileType(&NSString::from_str(&extension));
        #[expect(clippy::cast_precision_loss, reason = "a handful of files")]
        let offset = n as f64 * 8.0;
        let frame = NSRect::new(
            NSPoint::new(at.x - 16.0 + offset, at.y - 16.0 - offset),
            NSSize::new(32.0, 32.0),
        );
        // SAFETY: AppKit rule: the contents of a dragging frame may be an `NSImage`.
        unsafe {
            item.setDraggingFrame_contents(frame, Some(&icon));
        }
        keepers.push(keeper);
        items.push(item);
    }
    let mut givers = Vec::with_capacity(data.len());
    #[expect(deprecated, reason = "`iconForContentType:` needs UniformTypeIdentifiers")]
    let text_icon = NSWorkspace::sharedWorkspace().iconForFileType(&NSString::from_str("txt"));
    for data in data {
        let giver = Giver::new(data.give);
        let board_item = NSPasteboardItem::new();
        let types: Vec<Retained<NSString>> =
            data.types.iter().map(|t| NSString::from_str(t)).collect();
        let declared = board_item.setDataProvider_forTypes(
            ProtocolObject::from_ref(&*giver),
            &NSArray::from_retained_slice(&types),
        );
        if !declared {
            continue;
        }
        let writer: &ProtocolObject<dyn NSPasteboardWriting> =
            ProtocolObject::from_ref(&*board_item);
        let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
        #[expect(clippy::cast_precision_loss, reason = "a handful of items")]
        let offset = items.len() as f64 * 8.0;
        let frame = NSRect::new(
            NSPoint::new(at.x - 16.0 + offset, at.y - 16.0 - offset),
            NSSize::new(32.0, 32.0),
        );
        // SAFETY: AppKit rule: the contents of a dragging frame may be an `NSImage`.
        unsafe {
            item.setDraggingFrame_contents(frame, Some(&text_icon));
        }
        givers.push(giver);
        items.push(item);
    }
    let source = Source::new(mtm, tag);
    let session = view.beginDraggingSessionWithItems_event_source(
        &NSArray::from_retained_slice(&items),
        &event,
        ProtocolObject::from_ref(&*source),
    );
    tracing::info!(files = keepers.len(), data = givers.len(), ?session, "drag out");
    LIVE.with(|live| {
        let mut live = live.borrow_mut();
        live.push_back((source, keepers, givers));
        while live.len() > LIVE_DRAGS {
            live.pop_front();
        }
    });
    true
}

/// The tag of the drag `sender` carries when this app began it with one ([`drag_out_items`]):
/// AppKit names the source only to a destination in the source's own app.
pub(crate) fn own_tag(sender: &ProtocolObject<dyn NSDraggingInfo>) -> Option<u64> {
    let source = sender.draggingSource()?;
    let source = source.downcast::<Source>().ok()?;
    *source.ivars()
}

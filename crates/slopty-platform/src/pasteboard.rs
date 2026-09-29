//! The client's pasteboard behind a small trait: the system's general pasteboard in the app, a
//! uniquely named one in the self-tests, an in-memory one for pure logic.
//!
//! Reading another app's contents can ask the person first: on iOS unless a paste they started
//! does the reading, and on macOS unless they allowed it in System Settings
//! (`NSPasteboard.accessBehavior`, `pasteboard_access`). [`Pasteboard::reads_ask`] tells the
//! caller to read only on their intent (a paste into a remote tile). The change count is always
//! free to read.
//!
//! A write puts items on the pasteboard: the representations that are here now, and promises
//! for the rest, which the pasteboard asks [`Write::provide`] for when something pastes them
//! (on the main thread, and the bytes must be there before the call returns). Every write carries
//! [`ORIGIN_TYPE`], saying whose contents they are, so a reader can tell Slopty's writes from the
//! human's.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

/// The private type every Slopty write carries: who wrote it, and which generation
/// (`slopty_proto::transfer::origin_bytes`).
pub use slopty_proto::transfer::ORIGIN_TYPE;
/// Whether a type travels in an offer.
pub use slopty_proto::transfer::carried;
use slopty_proto::transfer::{ClipFormat, ClipType};

/// Marks contents a password manager copied: offered without bytes, never kept.
pub const CONCEALED_UTI: &str = slopty_proto::transfer::CONCEALED_TYPE;

/// Marks contents meant to live only briefly: treated as a secret is.
pub const TRANSIENT_UTI: &str = slopty_proto::transfer::TRANSIENT_TYPE;

/// Plain text.
pub const TEXT_UTI: &str = "public.utf8-plain-text";

/// A file's URL, one per item: what Finder's copy puts on the pasteboard.
pub const FILE_URL_UTI: &str = "public.file-url";

/// The pasteboard type clipboard sync's `format` has on Apple's pasteboards: its uniform type
/// identifier.
#[must_use]
pub const fn uti_of(format: ClipFormat) -> &'static str {
    match format {
        ClipFormat::FileUrls => FILE_URL_UTI,
        ClipFormat::Png => "public.png",
        ClipFormat::Tiff => "public.tiff",
        ClipFormat::Rtf => "public.rtf",
        ClipFormat::Html => "public.html",
        ClipFormat::Text => TEXT_UTI,
    }
}

/// The format a pasteboard type is, when it is one.
#[must_use]
pub fn format_of(uti: &str) -> Option<ClipFormat> {
    ClipFormat::ALL.into_iter().find(|&format| uti_of(format) == uti)
}

/// The wire type of a type on an Apple pasteboard: a format, else the UTI itself.
#[must_use]
pub fn clip_type(uti: &str) -> ClipType {
    format_of(uti).map_or_else(|| ClipType::Apple(uti.to_owned()), ClipType::Format)
}

/// The type `kind` has on an Apple pasteboard.
#[must_use]
pub fn uti_of_type(kind: &ClipType) -> &str {
    match kind {
        ClipType::Format(format) => uti_of(*format),
        ClipType::Apple(uti) => uti,
    }
}

/// Whether a representation names a file rather than holding contents.
///
/// That is a file's URL (by either spelling), Finder's node, a file promise's bookkeeping, or a
/// URL whose scheme is `file` (its `bytes`, when known). Such a name means a file on the machine
/// that wrote it; on another it means whatever sits at that path there, so it is never put on a
/// pasteboard as it is. Files travel as a transfer instead.
#[must_use]
pub fn names_a_file(uti: &str, bytes: Option<&[u8]>) -> bool {
    let file_type = uti.contains("file-url")
        || uti.starts_with("com.apple.finder.")
        || uti.starts_with("com.apple.pasteboard.promised-")
        || uti == "com.apple.NSFilePromiseItemMetaData";
    let file_link = uti == "public.url"
        && bytes.is_some_and(|b| {
            b.trim_ascii_start().get(..5).is_some_and(|s| s.eq_ignore_ascii_case(b"file:"))
        });
    file_type || file_link
}

/// Whether a representation another machine sent may go on this pasteboard as it is: a type
/// that travels, spelled however it was sent, that names no file here.
#[must_use]
pub fn writable(uti: &str, bytes: Option<&[u8]>) -> bool {
    (format_of(uti).is_some() || carried(uti)) && !names_a_file(uti, bytes)
}

/// A read of one representation, capped at a size.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Capped {
    /// Its bytes, no more than the cap.
    Data(Vec<u8>),
    /// Past the cap: its size, and none of its bytes kept.
    TooBig(u64),
}

/// Answers a promised representation of item `item` with its bytes, or nothing when they
/// cannot be had.
pub type Provide = Arc<dyn Fn(usize, &str) -> Option<Vec<u8>> + Send + Sync>;

/// One item of a write.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WriteItem {
    /// Representations put on now, by type.
    pub data: Vec<(String, Vec<u8>)>,
    /// Representations promised, answered by the write's `provide` when pasted.
    pub promised: Vec<String>,
}

impl std::fmt::Debug for WriteItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteItem")
            .field("data", &self.data.iter().map(|(t, b)| (t, b.len())).collect::<Vec<_>>())
            .field("promised", &self.promised)
            .finish()
    }
}

/// One write: its items, who wrote it, and what keeps its promises.
#[derive(Clone, Default)]
pub struct Write {
    /// The [`ORIGIN_TYPE`] payload, on the first item.
    pub origin: Vec<u8>,
    /// The items, in order.
    pub items: Vec<WriteItem>,
    /// Answers the promises.
    pub provide: Option<Provide>,
}

impl std::fmt::Debug for Write {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Write").field("items", &self.items).finish_non_exhaustive()
    }
}

/// A pasteboard.
pub trait Pasteboard {
    /// Bumped by every change, anyone's.
    fn change_count(&self) -> i64;
    /// The types of the first item, richest first.
    fn types(&self) -> Vec<String> {
        self.items().into_iter().next().unwrap_or_default()
    }
    /// The types of every item, in pasteboard order.
    fn items(&self) -> Vec<Vec<String>>;
    /// The bytes of one type of the first item (asking a promise's provider).
    fn data(&self, uti: &str) -> Option<Vec<u8>> {
        self.item_data(0, uti)
    }
    /// The bytes of type `uti` of item `item`. A file URL comes back as a path URL: a file
    /// named by reference means nothing on another machine.
    fn item_data(&self, item: usize, uti: &str) -> Option<Vec<u8>>;
    /// The bytes of type `uti` of item `item` when they are no more than `max`, else only their
    /// size. A pasteboard that can tell the size before copying copies nothing past the cap.
    fn item_data_within(&self, item: usize, uti: &str, max: u64) -> Option<Capped> {
        let bytes = self.item_data(item, uti)?;
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        Some(if size > max { Capped::TooBig(size) } else { Capped::Data(bytes) })
    }
    /// The `file://` URLs of the files the contents name, one per item that names one, each a
    /// path URL; empty when they name none.
    fn file_urls(&self) -> Vec<String> {
        (0..self.items().len())
            .filter_map(|n| self.item_data(n, FILE_URL_UTI))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .collect()
    }
    /// Replace the contents; returns the change count this write produced.
    fn write(&self, write: Write) -> i64;
    /// Whether reading the contents may ask the person first: read them only when they paste,
    /// never to keep something in step.
    fn reads_ask(&self) -> bool {
        false
    }
}

/// One item in [`Memory`].
#[derive(Debug, Default)]
struct MemItem {
    data: Vec<(String, Vec<u8>)>,
    promised: Vec<String>,
}

/// A pasteboard in memory, for tests of what is written and read.
#[derive(Debug, Default)]
pub struct Memory {
    count: Cell<i64>,
    items: RefCell<Vec<MemItem>>,
    provide: RefCell<Option<WriteProvide>>,
    asks: Cell<bool>,
    reads: Cell<usize>,
}

struct WriteProvide(Provide);

impl std::fmt::Debug for WriteProvide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Provide")
    }
}

impl Memory {
    /// One whose reads ask first, as iOS's general pasteboard does, and macOS's until the person
    /// allows reads.
    #[must_use]
    pub fn asking() -> Self {
        let board = Self::default();
        board.asks.set(true);
        board
    }

    /// Reads ask first (`true`), or no longer do.
    pub fn set_asks(&self, asks: bool) {
        self.asks.set(asks);
    }

    /// How many times contents were read ([`Pasteboard::item_data`]).
    #[must_use]
    pub const fn reads(&self) -> usize {
        self.reads.get()
    }

    /// Put `data` on it as one item, as a copy in another app does (no origin).
    pub fn copy(&self, data: &[(&str, &[u8])]) {
        self.copy_items(&[data]);
    }

    /// Put `items` on it as someone else would (no origin), as a copy in another app does.
    pub fn copy_items(&self, items: &[&[(&str, &[u8])]]) {
        self.clear();
        self.put_items(items);
    }

    /// The first half of another app's copy (`clearContents`): the count moves and nothing is
    /// on it yet.
    pub fn clear(&self) {
        self.items.borrow_mut().clear();
        *self.provide.borrow_mut() = None;
        self.count.set(self.count.get().saturating_add(1));
    }

    /// The second half (`writeObjects:`): `data` lands as one item under the count the clear
    /// left.
    pub fn put(&self, data: &[(&str, &[u8])]) {
        self.put_items(&[data]);
    }

    fn put_items(&self, items: &[&[(&str, &[u8])]]) {
        *self.items.borrow_mut() = items
            .iter()
            .map(|reps| MemItem {
                data: reps.iter().map(|(t, b)| ((*t).to_owned(), b.to_vec())).collect(),
                promised: Vec::new(),
            })
            .collect();
    }

    /// Put file URLs on it as Finder's copy does: one item per file, no origin.
    pub fn copy_files(&self, urls: &[&str]) {
        let items: Vec<[(&str, &[u8]); 1]> =
            urls.iter().map(|u| [(FILE_URL_UTI, u.as_bytes())]).collect();
        let items: Vec<&[(&str, &[u8])]> = items.iter().map(<[_; 1]>::as_slice).collect();
        self.copy_items(&items);
    }

    /// The types of the first item that are promises rather than bytes.
    #[must_use]
    pub fn promised(&self) -> Vec<String> {
        self.items.borrow().first().map(|i| i.promised.clone()).unwrap_or_default()
    }
}

impl Pasteboard for Memory {
    fn change_count(&self) -> i64 {
        self.count.get()
    }

    fn items(&self) -> Vec<Vec<String>> {
        self.items
            .borrow()
            .iter()
            .map(|i| {
                i.data.iter().map(|(t, _)| t.clone()).chain(i.promised.iter().cloned()).collect()
            })
            .collect()
    }

    fn item_data(&self, item: usize, uti: &str) -> Option<Vec<u8>> {
        self.reads.set(self.reads.get().saturating_add(1));
        {
            let items = self.items.borrow();
            let entry = items.get(item)?;
            if let Some((_, bytes)) = entry.data.iter().find(|(t, _)| t == uti) {
                return Some(bytes.clone());
            }
            if !entry.promised.iter().any(|t| t == uti) {
                return None;
            }
        }
        let provide = self.provide.borrow().as_ref().map(|p| Arc::clone(&p.0))?;
        provide(item, uti)
    }

    /// Read without counting, as the Mac's file URLs are: [`Memory::reads`] counts contents.
    fn file_urls(&self) -> Vec<String> {
        self.items
            .borrow()
            .iter()
            .filter_map(|i| i.data.iter().find(|(t, _)| t == FILE_URL_UTI))
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
            .collect()
    }

    fn write(&self, write: Write) -> i64 {
        let mut items: Vec<MemItem> = write
            .items
            .into_iter()
            .map(|i| MemItem { data: i.data, promised: i.promised })
            .collect();
        if let Some(first) = items.first_mut() {
            first.data.push((ORIGIN_TYPE.to_owned(), write.origin));
        }
        *self.items.borrow_mut() = items;
        *self.provide.borrow_mut() = write.provide.map(WriteProvide);
        self.count.set(self.count.get().saturating_add(1));
        self.count.get()
    }

    fn reads_ask(&self) -> bool {
        self.asks.get()
    }
}

#[cfg(target_os = "macos")]
pub use mac::MacPasteboard;

#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{AllocAnyThread as _, DefinedClass as _, define_class, msg_send};
    use objc2_app_kit::{
        NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardItemDataProvider,
        NSPasteboardType, NSPasteboardWriting,
    };
    use objc2_foundation::{NSArray, NSData, NSObject, NSObjectProtocol, NSString, NSURL};

    use super::{Capped, FILE_URL_UTI, ORIGIN_TYPE, Pasteboard, Provide, Write};

    struct Ivars {
        /// The item of the write this provider keeps the promises of.
        item: usize,
        provide: Provide,
    }

    define_class!(
        // SAFETY:
        // - `NSObject` has no subclassing requirements.
        // - `Provider` does not implement `Drop`.
        #[unsafe(super(NSObject))]
        #[name = "SloptyPasteboardProvider"]
        #[ivars = Ivars]
        struct Provider;

        unsafe impl NSObjectProtocol for Provider {}

        unsafe impl NSPasteboardItemDataProvider for Provider {
            #[unsafe(method(pasteboard:item:provideDataForType:))]
            fn provide_data(
                &self,
                _pasteboard: Option<&NSPasteboard>,
                item: &NSPasteboardItem,
                kind: &NSPasteboardType,
            ) {
                let uti = kind.to_string();
                let ivars = self.ivars();
                if let Some(bytes) = (ivars.provide)(ivars.item, &uti) {
                    let set = item.setData_forType(&NSData::with_bytes(&bytes), kind);
                    tracing::debug!(uti, bytes = bytes.len(), set, "promise kept");
                } else {
                    tracing::debug!(uti, "promise not kept");
                }
            }
        }
    );

    impl Provider {
        fn new(item: usize, provide: Provide) -> Retained<Self> {
            let this = Self::alloc().set_ivars(Ivars { item, provide });
            // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// An `NSPasteboard`: the general one, or one of its own name.
    #[derive(Debug)]
    pub struct MacPasteboard {
        board: Retained<NSPasteboard>,
        /// The providers of the last write's promises, kept alive until the next write.
        providers: std::cell::RefCell<Vec<Retained<Provider>>>,
    }

    impl std::fmt::Debug for Provider {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Provider")
        }
    }

    impl MacPasteboard {
        /// The system's general pasteboard: what ⌘C and ⌘V use in every app.
        #[must_use]
        pub fn general() -> Self {
            Self {
                board: NSPasteboard::generalPasteboard(),
                providers: std::cell::RefCell::default(),
            }
        }

        /// The pasteboard called `name`, made on first use and shared by every process that
        /// names it. A test's own: nothing else on the machine reads or writes it.
        #[must_use]
        pub fn named(name: &str) -> Self {
            let board = NSPasteboard::pasteboardWithName(&NSString::from_str(name));
            Self { board, providers: std::cell::RefCell::default() }
        }

        /// Give a named pasteboard back to the system (a test's, when it is done).
        pub fn release(self) {
            // SAFETY: AppKit rule: `releaseGlobally` takes no arguments and may be sent to any
            // pasteboard; after it the object must not be used, and `self` is consumed here.
            unsafe {
                let () = msg_send![&*self.board, releaseGlobally];
            }
        }

        /// Put `data` on it as a copy in another app does: no origin, no promises. Returns the
        /// change count the copy produced.
        pub fn copy(&self, data: &[(&str, &[u8])]) -> i64 {
            self.copy_items(&[data])
        }

        /// Put `items` on it as a copy of several things in another app does: no origin, no
        /// promises. Returns the change count the copy produced.
        pub fn copy_items(&self, items: &[&[(&str, &[u8])]]) -> i64 {
            self.board.clearContents();
            let writers: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
                .iter()
                .map(|data| {
                    let item = NSPasteboardItem::new();
                    for (uti, bytes) in *data {
                        item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(uti));
                    }
                    ProtocolObject::from_retained(item)
                })
                .collect();
            self.board.writeObjects(&NSArray::from_retained_slice(&writers));
            self.change_count()
        }

        /// Put the files at `paths` on it as Finder's copy does: one item per file holding its
        /// URL, no origin. Returns the change count the copy produced.
        pub fn copy_files(&self, paths: &[&std::path::Path]) -> i64 {
            self.board.clearContents();
            let writers: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
                .iter()
                .map(|path| {
                    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
                    let item = NSPasteboardItem::new();
                    if let Some(text) = url.absoluteString() {
                        item.setString_forType(&text, &NSString::from_str(FILE_URL_UTI));
                    }
                    ProtocolObject::from_retained(item)
                })
                .collect();
            self.board.writeObjects(&NSArray::from_retained_slice(&writers));
            self.change_count()
        }

        /// Its text, when it has any.
        #[must_use]
        pub fn text(&self) -> Option<String> {
            self.board.stringForType(&NSString::from_str(super::TEXT_UTI)).map(|s| s.to_string())
        }
    }

    impl Pasteboard for MacPasteboard {
        fn change_count(&self) -> i64 {
            i64::try_from(self.board.changeCount()).unwrap_or(i64::MAX)
        }

        fn types(&self) -> Vec<String> {
            self.board
                .types()
                .map(|types| types.iter().map(|t| t.to_string()).collect())
                .unwrap_or_default()
        }

        fn items(&self) -> Vec<Vec<String>> {
            let Some(items) = self.board.pasteboardItems() else { return Vec::new() };
            items.iter().map(|item| item.types().iter().map(|t| t.to_string()).collect()).collect()
        }

        fn data(&self, uti: &str) -> Option<Vec<u8>> {
            self.board.dataForType(&NSString::from_str(uti)).map(|d| d.to_vec())
        }

        fn item_data(&self, item: usize, uti: &str) -> Option<Vec<u8>> {
            let items = self.board.pasteboardItems()?;
            if item >= items.count() {
                return None;
            }
            let entry = items.objectAtIndex(item);
            let kind = NSString::from_str(uti);
            if uti == FILE_URL_UTI {
                // Finder names a file by reference (`file:///.file/id=…`); the path is what
                // travels.
                let text = entry.stringForType(&kind)?;
                let url = NSURL::URLWithString(&text)?;
                return Some(url.filePathURL()?.absoluteString()?.to_string().into_bytes());
            }
            entry.dataForType(&kind).map(|d| d.to_vec())
        }

        fn item_data_within(&self, item: usize, uti: &str, max: u64) -> Option<Capped> {
            if uti == FILE_URL_UTI {
                let bytes = self.item_data(item, uti)?;
                let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                return Some(if size > max { Capped::TooBig(size) } else { Capped::Data(bytes) });
            }
            let items = self.board.pasteboardItems()?;
            if item >= items.count() {
                return None;
            }
            // Its length is read before its bytes are copied: past the cap, none are kept.
            let data = items.objectAtIndex(item).dataForType(&NSString::from_str(uti))?;
            let size = u64::try_from(data.length()).unwrap_or(u64::MAX);
            Some(if size > max { Capped::TooBig(size) } else { Capped::Data(data.to_vec()) })
        }

        fn write(&self, write: Write) -> i64 {
            let Write { origin, items, provide } = write;
            let mut providers = Vec::new();
            let writers: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
                .iter()
                .enumerate()
                .map(|(n, reps)| {
                    let item = NSPasteboardItem::new();
                    for (uti, bytes) in &reps.data {
                        item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(uti));
                    }
                    if n == 0 {
                        item.setData_forType(
                            &NSData::with_bytes(&origin),
                            &NSString::from_str(ORIGIN_TYPE),
                        );
                    }
                    if let Some(provide) = provide.as_ref().filter(|_| !reps.promised.is_empty()) {
                        let provider = Provider::new(n, std::sync::Arc::clone(provide));
                        let types: Vec<Retained<NSString>> =
                            reps.promised.iter().map(|t| NSString::from_str(t)).collect();
                        let types = NSArray::from_retained_slice(&types);
                        item.setDataProvider_forTypes(ProtocolObject::from_ref(&*provider), &types);
                        providers.push(provider);
                    }
                    ProtocolObject::from_retained(item)
                })
                .collect();
            // Current host only: Universal Clipboard would otherwise fetch every promise at once
            // to hand the contents to the person's other devices.
            self.board
                .prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
            let wrote = self.board.writeObjects(&NSArray::from_retained_slice(&writers));
            if !wrote {
                tracing::warn!("pasteboard write refused");
            }
            *self.providers.borrow_mut() = providers;
            self.change_count()
        }

        /// The general pasteboard asks unless the person allowed this app to read it; a named
        /// one never asks.
        fn reads_ask(&self) -> bool {
            !crate::pasteboard_access::of(&self.board).reads_freely()
        }
    }
}

#[cfg(target_os = "ios")]
pub use ios::IosPasteboard;

#[cfg(target_os = "ios")]
mod ios {
    use std::ptr::NonNull;
    use std::sync::Arc;

    use block2::{DynBlock, RcBlock};
    use objc2::rc::Retained;
    use objc2_foundation::{
        NSArray, NSData, NSError, NSIndexSet, NSItemProvider,
        NSItemProviderRepresentationVisibility, NSProgress, NSString,
    };
    use objc2_ui_kit::UIPasteboard;

    use super::{Capped, ORIGIN_TYPE, Pasteboard, Provide, Write};

    /// A `UIPasteboard`: the general one, or one of its own name.
    #[derive(Debug)]
    pub struct IosPasteboard {
        board: Retained<UIPasteboard>,
        /// The name it was made with; `None` for the general one.
        name: Option<String>,
    }

    /// A representation handed over at once or fetched when a paste loads it: the item
    /// provider asks from a queue of its own and waits on the completion.
    fn register(
        provider: &NSItemProvider,
        uti: &str,
        load: impl Fn() -> Option<Vec<u8>> + Send + 'static,
    ) {
        let uti_name = uti.to_owned();
        let handler = RcBlock::new(
            move |done: NonNull<DynBlock<dyn Fn(*mut NSData, *mut NSError)>>| -> *mut NSProgress {
                // SAFETY: Foundation rule: the completion block the load handler gets is valid
                // until it is called, and it is called here, before the handler returns.
                let done = unsafe { done.as_ref() };
                if let Some(bytes) = load() {
                    let data = NSData::with_bytes(&bytes);
                    done.call((Retained::as_ptr(&data).cast_mut(), std::ptr::null_mut()));
                } else {
                    tracing::debug!(uti = %uti_name, "promise not kept");
                    let domain = NSString::from_str("com.aislopware.slopty");
                    // SAFETY: Foundation rule: any domain, any code and a nil user info make a
                    // valid error.
                    let error = unsafe { NSError::errorWithDomain_code_userInfo(&domain, 1, None) };
                    done.call((std::ptr::null_mut(), Retained::as_ptr(&error).cast_mut()));
                }
                // A null progress: the load is done by the time the handler returns.
                std::ptr::null_mut()
            },
        );
        // SAFETY: Foundation rule: a type identifier string, a visibility constant and a load
        // handler that calls its completion exactly once. The handler is called on a queue of
        // the system's choosing, which it may be: `load` is `Send`, and so is what it holds.
        unsafe {
            provider.registerDataRepresentationForTypeIdentifier_visibility_loadHandler(
                &NSString::from_str(uti),
                NSItemProviderRepresentationVisibility::All,
                &handler,
            );
        }
    }

    impl IosPasteboard {
        /// The system's general pasteboard: what Copy and Paste use in every app.
        #[must_use]
        pub fn general() -> Self {
            Self { board: UIPasteboard::generalPasteboard(), name: None }
        }

        /// The app's pasteboard called `name`, made on first use. A test's own: the person's
        /// clipboard is not touched.
        #[must_use]
        pub fn named(name: &str) -> Option<Self> {
            let board = UIPasteboard::pasteboardWithName_create(&NSString::from_str(name), true)?;
            Some(Self { board, name: Some(name.to_owned()) })
        }

        /// Give a named pasteboard back to the system (a test's, when it is done).
        pub fn release(self) {
            if let Some(name) = &self.name {
                UIPasteboard::removePasteboardWithName(&NSString::from_str(name));
            }
        }

        /// Put `data` on it as a copy in another app does: no origin, no promises. Returns the
        /// change count the copy produced.
        pub fn copy(&self, data: &[(&str, &[u8])]) -> i64 {
            let provider = NSItemProvider::new();
            for (uti, bytes) in data {
                let bytes = bytes.to_vec();
                register(&provider, uti, move || Some(bytes.clone()));
            }
            self.board.setItemProviders_localOnly_expirationDate(
                &NSArray::from_retained_slice(&[provider]),
                true,
                None,
            );
            self.change_count()
        }

        /// Its text, when it has any.
        #[must_use]
        pub fn text(&self) -> Option<String> {
            // SAFETY: UIKit rule: `string` reads the pasteboard's text from any thread; nil
            // when it holds none, which `Option` covers.
            unsafe { self.board.string() }.map(|s| s.to_string())
        }
    }

    impl Pasteboard for IosPasteboard {
        fn change_count(&self) -> i64 {
            // SAFETY: UIKit rule: `changeCount` is a counter any thread may read, and reading
            // it never shows the paste prompt.
            i64::try_from(unsafe { self.board.changeCount() }).unwrap_or(i64::MAX)
        }

        fn types(&self) -> Vec<String> {
            // SAFETY: UIKit rule: `pasteboardTypes` lists the first item's types from any
            // thread, without its contents.
            unsafe { self.board.pasteboardTypes() }.iter().map(|t| t.to_string()).collect()
        }

        /// Every item's types, read without their contents, as `pasteboardTypes` is.
        fn items(&self) -> Vec<Vec<String>> {
            self.board.pasteboardTypesForItemSet(None).map_or_else(Vec::new, |items| {
                items.iter().map(|types| types.iter().map(|t| t.to_string()).collect()).collect()
            })
        }

        fn data(&self, uti: &str) -> Option<Vec<u8>> {
            self.board.dataForPasteboardType(&NSString::from_str(uti)).map(|d| d.to_vec())
        }

        fn item_data(&self, item: usize, uti: &str) -> Option<Vec<u8>> {
            let set = NSIndexSet::indexSetWithIndex(item);
            let found =
                self.board.dataForPasteboardType_inItemSet(&NSString::from_str(uti), Some(&set))?;
            found.firstObject().map(|d| d.to_vec())
        }

        fn item_data_within(&self, item: usize, uti: &str, max: u64) -> Option<Capped> {
            let set = NSIndexSet::indexSetWithIndex(item);
            let found =
                self.board.dataForPasteboardType_inItemSet(&NSString::from_str(uti), Some(&set))?;
            let data = found.firstObject()?;
            // Its length is read before its bytes are copied: past the cap, none are kept.
            let size = u64::try_from(data.length()).unwrap_or(u64::MAX);
            Some(if size > max { Capped::TooBig(size) } else { Capped::Data(data.to_vec()) })
        }

        /// One item provider per item: the representations here now handed over at once, the
        /// promised ones loaded through `provide` when something pastes them. Local only, so
        /// the system's Universal Clipboard does not fetch every promise to hand it to another
        /// device.
        fn write(&self, write: Write) -> i64 {
            let Write { origin, items, provide } = write;
            let providers: Vec<Retained<NSItemProvider>> = items
                .into_iter()
                .enumerate()
                .map(|(n, item)| {
                    let provider = NSItemProvider::new();
                    for (uti, bytes) in item.data {
                        register(&provider, &uti, move || Some(bytes.clone()));
                    }
                    if n == 0 {
                        let origin = origin.clone();
                        register(&provider, ORIGIN_TYPE, move || Some(origin.clone()));
                    }
                    if let Some(provide) = &provide {
                        for uti in item.promised {
                            let (provide, asked): (Provide, String) =
                                (Arc::clone(provide), uti.clone());
                            register(&provider, &uti, move || provide(n, &asked));
                        }
                    }
                    provider
                })
                .collect();
            self.board.setItemProviders_localOnly_expirationDate(
                &NSArray::from_retained_slice(&providers),
                true,
                None,
            );
            self.change_count()
        }

        /// None: files come to an iPhone or iPad by the Files picker or a drop, and reading
        /// them off the clipboard would ask the person.
        fn file_urls(&self) -> Vec<String> {
            Vec::new()
        }

        fn reads_ask(&self) -> bool {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The named pasteboard behaves as the general one does, and nothing here touches the
    /// general one: text now, a promise answered when read, the origin stamp, the count moving.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_named_pasteboard_keeps_text_promises_and_the_origin() {
        let name = format!("com.aislopware.slopty.test.{}", std::process::id());
        let board = MacPasteboard::named(&name);
        let before = board.change_count();
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        let count = board.write(Write {
            origin: b"worker:7".to_vec(),
            items: vec![
                WriteItem {
                    data: vec![(TEXT_UTI.to_owned(), b"hello".to_vec())],
                    promised: vec!["public.png".to_owned()],
                },
                WriteItem { data: Vec::new(), promised: vec!["com.adobe.pdf".to_owned()] },
            ],
            provide: Some(Arc::new(move |item, uti| {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                match (item, uti) {
                    (0, "public.png") => Some(vec![0x89, b'P', b'N', b'G']),
                    (1, "com.adobe.pdf") => Some(b"%PDF".to_vec()),
                    _ => None,
                }
            })),
        });
        assert!(count > before, "a write moves the count");
        assert_eq!(board.change_count(), count, "and nothing else did");
        assert_eq!(board.text().as_deref(), Some("hello"));
        assert!(board.types().iter().any(|t| t == ORIGIN_TYPE), "{:?}", board.types());
        assert_eq!(board.data(ORIGIN_TYPE).as_deref(), Some(&b"worker:7"[..]));
        assert_eq!(board.data("public.png"), Some(vec![0x89, b'P', b'N', b'G']));
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1, "asked once, on read");
        assert_eq!(board.items().len(), 2, "{:?}", board.items());
        assert_eq!(board.item_data(1, "com.adobe.pdf").as_deref(), Some(&b"%PDF"[..]));
        assert!(!board.reads_ask(), "a named pasteboard never asks");
        board.release();
    }

    /// Files copied as Finder copies them come back as path URLs, one per item, a file named
    /// by reference included; the general pasteboard is never touched.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_named_pasteboard_names_the_files_copied_on_it() {
        use objc2_foundation::{NSString, NSURL};

        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a b.txt"), dir.path().join("c.txt"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"c").unwrap();
        let name = format!("com.aislopware.slopty.test.files.{}", std::process::id());
        let board = MacPasteboard::named(&name);
        board.copy_files(&[a.as_path(), b.as_path()]);
        let urls = board.file_urls();
        assert_eq!(urls.len(), 2, "{urls:?}");
        assert!(urls[0].starts_with("file:///") && urls[0].ends_with("/a%20b.txt"), "{urls:?}");

        let url = NSURL::fileURLWithPath(&NSString::from_str(&b.to_string_lossy()));
        let by_reference = url.fileReferenceURL().unwrap().absoluteString().unwrap().to_string();
        assert!(by_reference.contains("/.file/id="), "{by_reference}");
        board.copy(&[(FILE_URL_UTI, by_reference.as_bytes())]);
        let urls = board.file_urls();
        assert!(urls.len() == 1 && urls[0].ends_with("/c.txt"), "resolved: {urls:?}");
        board.release();
    }

    /// Every way a pasteboard names a file is told apart from contents, whatever its spelling;
    /// a web link, text and a picture are contents.
    #[test]
    fn a_file_named_any_way_is_not_contents() {
        for uti in [
            FILE_URL_UTI,
            "com.apple.finder.node",
            "com.apple.pasteboard.promised-file-url",
            "com.apple.pasteboard.promised-file-content-type",
            "com.apple.NSFilePromiseItemMetaData",
        ] {
            assert!(names_a_file(uti, None), "{uti}");
            assert!(!writable(uti, None), "{uti}");
        }
        assert!(names_a_file("public.url", Some(b"file:///Users/me/.ssh/id_ed25519")));
        assert!(names_a_file("public.url", Some(b"  FILE:///etc/passwd")));
        assert!(!names_a_file("public.url", Some(b"https://example.com")));
        for uti in [TEXT_UTI, "public.png", "com.adobe.pdf"] {
            assert!(writable(uti, None), "{uti}");
        }
        for uti in [ORIGIN_TYPE, CONCEALED_UTI, "dyn.ah62d4rv4gu8y", "NSStringPboardType"] {
            assert!(!writable(uti, None), "{uti} never comes from elsewhere");
        }
    }

    #[test]
    fn memory_answers_promises_and_stamps_every_write() {
        let board = Memory::default();
        board.copy(&[(TEXT_UTI, b"typed")]);
        assert_eq!(board.change_count(), 1);
        assert!(!board.types().iter().any(|t| t == ORIGIN_TYPE), "someone else's copy");
        let count = board.write(Write {
            origin: b"me".to_vec(),
            items: vec![
                WriteItem { promised: vec!["public.tiff".to_owned()], ..WriteItem::default() },
                WriteItem { promised: vec!["public.png".to_owned()], ..WriteItem::default() },
            ],
            provide: Some(Arc::new(|item, _| Some(vec![u8::try_from(item).unwrap_or(9)]))),
        });
        assert_eq!(count, 2);
        assert_eq!(board.data("public.tiff"), Some(vec![0]));
        assert_eq!(board.item_data(1, "public.png"), Some(vec![1]), "asked with its item");
        assert_eq!(board.data(TEXT_UTI), None, "replaced");
        assert_eq!(board.data(ORIGIN_TYPE), Some(b"me".to_vec()));
        assert_eq!(board.reads(), 4, "each read counted");
        board.copy_files(&["file:///a", "file:///b"]);
        assert_eq!(board.file_urls(), ["file:///a", "file:///b"], "one item per file");
        assert!(!board.reads_ask() && Memory::asking().reads_ask());
    }
}

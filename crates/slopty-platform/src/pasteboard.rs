//! The client's pasteboard behind a small trait: the system's general pasteboard in the app, a
//! uniquely named one in the self-tests, an in-memory one for pure logic.
//!
//! On iOS reading another app's contents shows the paste prompt unless a paste the person
//! started does the reading, so [`Pasteboard::reads_ask`] tells the caller to read only on
//! their intent (a paste into a remote tile). The change count is always free to read.
//!
//! A write puts one item on the pasteboard: the representations that are here now, and promises
//! for the rest, which the pasteboard asks [`Write::provide`] for when something pastes them
//! (on the main thread, and the bytes must be there before the call returns). Every write carries
//! [`ORIGIN_TYPE`], saying whose contents they are, so a reader can tell Slopty's writes from the
//! human's.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The private type every Slopty write carries: who wrote it, and which generation
/// (`slopty_proto::transfer::origin_bytes`).
pub use slopty_proto::transfer::ORIGIN_TYPE;

/// Marks contents a password manager copied: never read, never synced.
pub const CONCEALED_UTI: &str = "org.nspasteboard.ConcealedType";

/// Marks contents meant to live only briefly: never synced.
pub const TRANSIENT_UTI: &str = "org.nspasteboard.TransientType";

/// Plain text.
pub const TEXT_UTI: &str = "public.utf8-plain-text";

/// A file's URL, one per item: what Finder's copy puts on the pasteboard.
pub const FILE_URL_UTI: &str = "public.file-url";

/// Answers a promised representation with its bytes, or nothing when it cannot be had.
pub type Provide = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// One write: what is here now, what is promised, and who wrote it.
#[derive(Clone, Default)]
pub struct Write {
    /// The [`ORIGIN_TYPE`] payload.
    pub origin: Vec<u8>,
    /// Representations put on now, by type.
    pub data: Vec<(String, Vec<u8>)>,
    /// Representations promised, answered by `provide` when pasted.
    pub promised: Vec<String>,
    /// Answers the promises.
    pub provide: Option<Provide>,
}

impl std::fmt::Debug for Write {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Write")
            .field("data", &self.data.iter().map(|(t, b)| (t, b.len())).collect::<Vec<_>>())
            .field("promised", &self.promised)
            .finish_non_exhaustive()
    }
}

/// A pasteboard.
pub trait Pasteboard {
    /// Bumped by every change, anyone's.
    fn change_count(&self) -> i64;
    /// The types of the current contents, richest first.
    fn types(&self) -> Vec<String>;
    /// The bytes of one type of the current contents (asking a promise's provider).
    fn data(&self, uti: &str) -> Option<Vec<u8>>;
    /// The `file://` URLs of the files the contents name, one per item that names one, each a
    /// path URL (a file reference URL resolved); empty when they name none.
    fn file_urls(&self) -> Vec<String> {
        Vec::new()
    }
    /// Replace the contents; returns the change count this write produced.
    fn write(&self, write: Write) -> i64;
    /// Whether reading the contents may ask the person first (iOS): read them only when they
    /// paste, never to keep something in step.
    fn reads_ask(&self) -> bool {
        false
    }
}

/// A pasteboard in memory, for tests of what is written and read.
#[derive(Debug, Default)]
pub struct Memory {
    count: Cell<i64>,
    now: RefCell<BTreeMap<String, Vec<u8>>>,
    promised: RefCell<Vec<String>>,
    provide: RefCell<Option<WriteProvide>>,
    order: RefCell<Vec<String>>,
    files: RefCell<Vec<String>>,
    asks: bool,
    reads: Cell<usize>,
}

struct WriteProvide(Provide);

impl std::fmt::Debug for WriteProvide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Provide")
    }
}

impl Memory {
    /// One whose reads ask first, as iOS's general pasteboard does.
    #[must_use]
    pub fn asking() -> Self {
        Self { asks: true, ..Self::default() }
    }

    /// How many times contents were read ([`Pasteboard::data`]).
    #[must_use]
    pub const fn reads(&self) -> usize {
        self.reads.get()
    }

    /// Put `data` on it as someone else would (no origin), as a copy in another app does.
    pub fn copy(&self, data: &[(&str, &[u8])]) {
        self.write(Write {
            data: data.iter().map(|(t, b)| ((*t).to_owned(), b.to_vec())).collect(),
            ..Write::default()
        });
        self.now.borrow_mut().remove(ORIGIN_TYPE);
        self.order.borrow_mut().retain(|t| t != ORIGIN_TYPE);
    }

    /// Put file URLs on it as Finder's copy does: one item per file, no origin.
    pub fn copy_files(&self, urls: &[&str]) {
        let first = urls.first().map(|u| u.as_bytes()).unwrap_or_default();
        self.copy(&[(FILE_URL_UTI, first)]);
        *self.files.borrow_mut() = urls.iter().map(|u| (*u).to_owned()).collect();
    }

    /// The types that are promises rather than bytes.
    #[must_use]
    pub fn promised(&self) -> Vec<String> {
        self.promised.borrow().clone()
    }
}

impl Pasteboard for Memory {
    fn change_count(&self) -> i64 {
        self.count.get()
    }

    fn types(&self) -> Vec<String> {
        self.order.borrow().clone()
    }

    fn data(&self, uti: &str) -> Option<Vec<u8>> {
        self.reads.set(self.reads.get().saturating_add(1));
        if let Some(bytes) = self.now.borrow().get(uti) {
            return Some(bytes.clone());
        }
        if !self.promised.borrow().iter().any(|t| t == uti) {
            return None;
        }
        let provide = self.provide.borrow().as_ref().map(|p| Arc::clone(&p.0))?;
        provide(uti)
    }

    fn file_urls(&self) -> Vec<String> {
        self.files.borrow().clone()
    }

    fn write(&self, write: Write) -> i64 {
        self.files.borrow_mut().clear();
        let mut now = BTreeMap::new();
        let mut order = Vec::new();
        for (uti, bytes) in write.data {
            order.push(uti.clone());
            now.insert(uti, bytes);
        }
        order.extend(write.promised.iter().cloned());
        order.push(ORIGIN_TYPE.to_owned());
        now.insert(ORIGIN_TYPE.to_owned(), write.origin);
        *self.now.borrow_mut() = now;
        *self.order.borrow_mut() = order;
        *self.promised.borrow_mut() = write.promised;
        *self.provide.borrow_mut() = write.provide.map(WriteProvide);
        self.count.set(self.count.get().saturating_add(1));
        self.count.get()
    }

    fn reads_ask(&self) -> bool {
        self.asks
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
        NSPasteboard, NSPasteboardItem, NSPasteboardItemDataProvider, NSPasteboardType,
        NSPasteboardWriting,
    };
    use objc2_foundation::{NSArray, NSData, NSObject, NSObjectProtocol, NSString, NSURL};

    use super::{ORIGIN_TYPE, Pasteboard, Provide, Write};

    struct Ivars {
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
                if let Some(bytes) = (self.ivars().provide)(&uti) {
                    let set = item.setData_forType(&NSData::with_bytes(&bytes), kind);
                    tracing::debug!(uti, bytes = bytes.len(), set, "promise kept");
                } else {
                    tracing::debug!(uti, "promise not kept");
                }
            }
        }
    );

    impl Provider {
        fn new(provide: Provide) -> Retained<Self> {
            let this = Self::alloc().set_ivars(Ivars { provide });
            // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// An `NSPasteboard`: the general one, or one of its own name.
    #[derive(Debug)]
    pub struct MacPasteboard {
        board: Retained<NSPasteboard>,
        /// The provider of the last write's promises, kept alive until the next write.
        provider: std::cell::RefCell<Option<Retained<Provider>>>,
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
                provider: std::cell::RefCell::default(),
            }
        }

        /// The pasteboard called `name`, made on first use and shared by every process that
        /// names it. A test's own: nothing else on the machine reads or writes it.
        #[must_use]
        pub fn named(name: &str) -> Self {
            let board = NSPasteboard::pasteboardWithName(&NSString::from_str(name));
            Self { board, provider: std::cell::RefCell::default() }
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
            self.board.clearContents();
            let item = NSPasteboardItem::new();
            for (uti, bytes) in data {
                item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(uti));
            }
            let writer: Retained<ProtocolObject<dyn NSPasteboardWriting>> =
                ProtocolObject::from_retained(item);
            self.board.writeObjects(&NSArray::from_retained_slice(&[writer]));
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
                        item.setString_forType(&text, &NSString::from_str(super::FILE_URL_UTI));
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

        fn data(&self, uti: &str) -> Option<Vec<u8>> {
            self.board.dataForType(&NSString::from_str(uti)).map(|d| d.to_vec())
        }

        fn file_urls(&self) -> Vec<String> {
            let Some(items) = self.board.pasteboardItems() else { return Vec::new() };
            let kind = NSString::from_str(super::FILE_URL_UTI);
            items
                .iter()
                .filter_map(|item| {
                    let text = item.stringForType(&kind)?;
                    let url = NSURL::URLWithString(&text)?;
                    // Finder names a file by reference (`file:///.file/id=…`); the path is what
                    // travels.
                    Some(url.filePathURL()?.absoluteString()?.to_string())
                })
                .collect()
        }

        fn write(&self, write: Write) -> i64 {
            self.board.clearContents();
            let item = NSPasteboardItem::new();
            for (uti, bytes) in &write.data {
                item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(uti));
            }
            item.setData_forType(
                &NSData::with_bytes(&write.origin),
                &NSString::from_str(ORIGIN_TYPE),
            );
            let provider = match write.provide {
                Some(provide) if !write.promised.is_empty() => {
                    let provider = Provider::new(provide);
                    let types: Vec<Retained<NSString>> =
                        write.promised.iter().map(|t| NSString::from_str(t)).collect();
                    let types = NSArray::from_retained_slice(&types);
                    item.setDataProvider_forTypes(ProtocolObject::from_ref(&*provider), &types);
                    Some(provider)
                }
                _ => None,
            };
            let writer: Retained<ProtocolObject<dyn NSPasteboardWriting>> =
                ProtocolObject::from_retained(item);
            let wrote = self.board.writeObjects(&NSArray::from_retained_slice(&[writer]));
            if !wrote {
                tracing::warn!("pasteboard write refused");
            }
            *self.provider.borrow_mut() = provider;
            self.change_count()
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
        NSArray, NSData, NSError, NSItemProvider, NSItemProviderRepresentationVisibility,
        NSProgress, NSString,
    };
    use objc2_ui_kit::UIPasteboard;

    use super::{ORIGIN_TYPE, Pasteboard, Provide, Write};

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

        fn data(&self, uti: &str) -> Option<Vec<u8>> {
            self.board.dataForPasteboardType(&NSString::from_str(uti)).map(|d| d.to_vec())
        }

        /// One item provider: the representations here now handed over at once, the promised
        /// ones loaded through `provide` when something pastes them. Local only, so the
        /// system's Universal Clipboard does not fetch every promise to hand it to another
        /// device.
        fn write(&self, write: Write) -> i64 {
            let provider = NSItemProvider::new();
            let Write { origin, data, promised, provide } = write;
            for (uti, bytes) in data {
                register(&provider, &uti, move || Some(bytes.clone()));
            }
            register(&provider, ORIGIN_TYPE, move || Some(origin.clone()));
            if let Some(provide) = provide {
                for uti in promised {
                    let (provide, asked): (Provide, String) = (Arc::clone(&provide), uti.clone());
                    register(&provider, &uti, move || provide(&asked));
                }
            }
            self.board.setItemProviders_localOnly_expirationDate(
                &NSArray::from_retained_slice(&[provider]),
                true,
                None,
            );
            self.change_count()
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
            data: vec![(TEXT_UTI.to_owned(), b"hello".to_vec())],
            promised: vec!["public.png".to_owned()],
            provide: Some(Arc::new(move |uti| {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                (uti == "public.png").then(|| vec![0x89, b'P', b'N', b'G'])
            })),
        });
        assert!(count > before, "a write moves the count");
        assert_eq!(board.change_count(), count, "and nothing else did");
        assert_eq!(board.text().as_deref(), Some("hello"));
        assert!(board.types().iter().any(|t| t == ORIGIN_TYPE), "{:?}", board.types());
        assert_eq!(board.data(ORIGIN_TYPE).as_deref(), Some(&b"worker:7"[..]));
        assert_eq!(board.data("public.png"), Some(vec![0x89, b'P', b'N', b'G']));
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1, "asked once, on read");
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

    #[test]
    fn memory_answers_promises_and_stamps_every_write() {
        let board = Memory::default();
        board.copy(&[(TEXT_UTI, b"typed")]);
        assert_eq!(board.change_count(), 1);
        assert!(!board.types().iter().any(|t| t == ORIGIN_TYPE), "someone else's copy");
        let count = board.write(Write {
            origin: b"me".to_vec(),
            promised: vec!["public.tiff".to_owned()],
            provide: Some(Arc::new(|_| Some(vec![1]))),
            ..Write::default()
        });
        assert_eq!(count, 2);
        assert_eq!(board.data("public.tiff"), Some(vec![1]));
        assert_eq!(board.data(TEXT_UTI), None, "replaced");
        assert_eq!(board.data(ORIGIN_TYPE), Some(b"me".to_vec()));
        assert_eq!(board.reads(), 3, "each read counted");
        assert!(!board.reads_ask() && Memory::asking().reads_ask());
    }
}

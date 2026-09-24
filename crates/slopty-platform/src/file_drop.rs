//! Files dropped from apps that promise them rather than name them.
//!
//! A Finder drop names files, and GPUI hands those to the tile under the pointer. Mail and
//! Photos drag file promises instead (`NSFilePromiseReceiver`): the file is written only once
//! the drop names a directory. On iPad every drop is item providers (`UIDropInteraction`).
//! Both land here in a fresh temporary directory ([`Landing`]). The files that arrived whole go
//! to the sink as one [`Dropped`], with where the drop was, for the app to hand to the tile
//! there as an ordinary file drop, which uploads them. A file that failed is reported by name
//! and left out, and whatever it wrote is deleted.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Files a drop brought here, ready to upload.
#[derive(Clone, Debug, PartialEq)]
pub struct Dropped {
    /// The files that arrived whole, in the order they did.
    pub paths: Vec<PathBuf>,
    /// The files that did not, each as `name: why`.
    pub failed: Vec<String>,
    /// Where the drop was, in the GPUI view's points from its top left.
    pub x: f64,
    /// See `x`.
    pub y: f64,
}

/// Where each drop's files are received: a directory of its own under the system's temporary
/// directory, which the system clears of files nobody touched for days.
#[must_use]
pub fn root() -> PathBuf {
    std::env::temp_dir().join("slopty-drops")
}

/// One drop's files arriving, each resolved once: whole, or failed.
#[derive(Debug)]
pub struct Landing {
    dir: PathBuf,
    expected: usize,
    resolved: usize,
    paths: Vec<PathBuf>,
    failed: Vec<String>,
    at: (f64, f64),
}

impl Landing {
    /// A fresh directory under `root` for a drop of `expected` files at `at`.
    pub fn new(root: &Path, expected: usize, at: (f64, f64)) -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(root)?;
        let dir = loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = root.join(format!("{}-{n}", std::process::id()));
            #[expect(clippy::create_dir, reason = "an existing directory is another drop's")]
            let made = std::fs::create_dir(&dir);
            match made {
                Ok(()) => break dir,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        };
        Ok(Self { dir, expected, resolved: 0, paths: Vec::new(), failed: Vec::new(), at })
    }

    /// The directory the files go to.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// One file resolved: whole at a path, or failed, with whatever it wrote (removed here) and
    /// why. The drop, once every file has resolved.
    pub fn resolve(
        &mut self,
        outcome: Result<PathBuf, (Option<PathBuf>, String)>,
    ) -> Option<Dropped> {
        match outcome {
            Ok(path) => self.paths.push(path),
            Err((partial, why)) => {
                let name = partial
                    .as_deref()
                    .and_then(Path::file_name)
                    .map_or_else(|| "A file".to_owned(), |n| n.to_string_lossy().into_owned());
                if let Some(partial) = partial.filter(|p| p.starts_with(&self.dir)) {
                    let _gone = std::fs::remove_file(partial);
                }
                tracing::warn!(%name, %why, "a dropped file did not arrive");
                self.failed.push(format!("{name}: {why}"));
            }
        }
        self.resolved = self.resolved.saturating_add(1);
        (self.resolved >= self.expected).then(|| Dropped {
            paths: std::mem::take(&mut self.paths),
            failed: std::mem::take(&mut self.failed),
            x: self.at.0,
            y: self.at.1,
        })
    }
}

/// Where a view's drops go.
type Sink = Rc<dyn Fn(Dropped)>;

thread_local! {
    /// Each host view's sink, by the view's address, and what keeps its receiver alive.
    static SINKS: RefCell<Vec<(usize, Sink, Keep)>> = RefCell::default();
}

#[cfg(target_os = "macos")]
type Keep = objc2::rc::Retained<macos::DropView>;
#[cfg(target_os = "ios")]
type Keep =
    (objc2::rc::Retained<ios::Delegate>, objc2::rc::Retained<objc2_ui_kit::UIDropInteraction>);

/// Hand a finished drop to its view's sink, on the main thread.
fn deliver(host: usize, dropped: Dropped) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        let sink = SINKS
            .with(|s| s.borrow().iter().find(|(h, ..)| *h == host).map(|(_, s, _)| Rc::clone(s)));
        if let Some(sink) = sink {
            sink(dropped);
        } else {
            tracing::warn!(files = dropped.paths.len(), "a drop for a view that is gone");
        }
    });
}

/// Receive promised files dropped on `host` and hand each drop to `sink` on the main thread.
///
/// `host` is the view a GPUI window draws into (its `raw_window_handle` handle). Once per view:
/// `false` off the main thread or when already done.
pub fn install(host: NonNull<c_void>, sink: Rc<dyn Fn(Dropped)>) -> bool {
    let key = host.as_ptr() as usize;
    if SINKS.with(|s| s.borrow().iter().any(|(h, ..)| *h == key)) {
        return false;
    }
    #[cfg(target_os = "macos")]
    let keep = macos::install(host, key);
    #[cfg(target_os = "ios")]
    let keep = ios::install(host, key);
    let Some(keep) = keep else { return false };
    SINKS.with(|s| s.borrow_mut().push((key, sink, keep)));
    true
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::ptr::NonNull;
    use std::sync::Arc;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, NSObjectProtocol, ProtocolObject};
    use objc2::{
        ClassType as _, DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    };
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSDragOperation, NSDraggingDestination, NSDraggingInfo,
        NSFilePromiseReceiver, NSResponder, NSView, NSWindowOrderingMode,
    };
    use objc2_foundation::{
        NSArray, NSDictionary, NSError, NSObject, NSOperationQueue, NSPoint, NSString, NSURL,
    };
    use parking_lot::Mutex;

    use super::{Landing, deliver, root};

    pub struct Ivars {
        host: usize,
        /// Where the promised files are written from, off the main thread, one at a time.
        queue: Retained<NSOperationQueue>,
    }

    define_class!(
        // SAFETY:
        // - `NSView` may be subclassed; the overrides keep its contracts (a hit test that
        //   finds nothing, a dragging destination).
        // - `DropView` does not implement `Drop`.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SloptyPromiseDropView"]
        #[ivars = Ivars]
        pub struct DropView;

        impl DropView {
            // Clicks and scrolls go to the GPUI view beneath; AppKit finds a drop's
            // destination by the types views registered, not by this.
            #[unsafe(method_id(hitTest:))]
            fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
                None
            }
        }

        unsafe impl NSObjectProtocol for DropView {}

        unsafe impl NSDraggingDestination for DropView {
            #[unsafe(method(draggingEntered:))]
            fn dragging_entered(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                NSDragOperation::Copy
            }

            #[unsafe(method(draggingUpdated:))]
            fn dragging_updated(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                NSDragOperation::Copy
            }

            #[unsafe(method(performDragOperation:))]
            fn perform(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
                self.receive(sender)
            }
        }
    );

    impl std::fmt::Debug for DropView {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("DropView")
        }
    }

    impl DropView {
        /// Receive the drop's promises into a landing of their own; `false` when it has none.
        fn receive(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let pasteboard = sender.draggingPasteboard();
            // A drag that names its files as well goes on with those names: nothing to copy.
            let named: Vec<PathBuf> = pasteboard
                .pasteboardItems()
                .map(|items| {
                    let url_type = NSString::from_str("public.file-url");
                    items
                        .iter()
                        .filter_map(|item| NSURL::URLWithString(&*item.stringForType(&url_type)?))
                        .filter_map(|url| url.path().map(|p| PathBuf::from(p.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            if !named.is_empty() {
                let (x, y) = self.location(sender);
                deliver(
                    self.ivars().host,
                    super::Dropped { paths: named, failed: Vec::new(), x, y },
                );
                return true;
            }
            let class: &AnyClass = NSFilePromiseReceiver::class();
            let classes = NSArray::from_slice(&[class]);
            // SAFETY: AppKit rule: an array of classes that adopt `NSPasteboardReading` and no
            // options; what comes back is instances of those classes.
            let objects = unsafe { pasteboard.readObjectsForClasses_options(&classes, None) };
            let receivers: Vec<Retained<NSFilePromiseReceiver>> = objects
                .map(|o| {
                    o.iter().filter_map(|o| o.downcast::<NSFilePromiseReceiver>().ok()).collect()
                })
                .unwrap_or_default();
            if receivers.is_empty() {
                return false;
            }
            let expected = receivers.iter().map(|r| r.fileTypes().count().max(1)).sum();
            let at = self.location(sender);
            let landing = match Landing::new(&root(), expected, at) {
                Ok(landing) => landing,
                Err(e) => {
                    tracing::warn!(error = %e, "no directory for a drop");
                    return false;
                }
            };
            let Some(dir) = landing.dir().to_str().map(NSString::from_str) else { return false };
            let dir = NSURL::fileURLWithPath_isDirectory(&dir, true);
            tracing::info!(files = expected, dir = %landing.dir().display(), "promised files dropped");
            let landing = Arc::new(Mutex::new(landing));
            let host = self.ivars().host;
            for receiver in receivers {
                let landing = Arc::clone(&landing);
                let reader = RcBlock::new(move |url: NonNull<NSURL>, error: *mut NSError| {
                    // SAFETY: AppKit rule: the reader gets a valid file URL for the call.
                    let url = unsafe { url.as_ref() };
                    // SAFETY: AppKit rule: the error is null or valid for the call.
                    let error = unsafe { error.as_ref() };
                    let path = url.path().map(|p| PathBuf::from(p.to_string()));
                    let outcome = match (path, error) {
                        (Some(path), None) => Ok(path),
                        (path, Some(error)) => {
                            Err((path, error.localizedDescription().to_string()))
                        }
                        (None, None) => Err((None, "not a file".to_owned())),
                    };
                    let dropped = landing.lock().resolve(outcome);
                    if let Some(dropped) = dropped {
                        deliver(host, dropped);
                    }
                });
                // SAFETY: AppKit rule: a directory URL, empty options, an operation queue that
                // outlives the call (this view's) and a reader called once per file on it,
                // which it may be: the reader holds only `Send` values.
                unsafe {
                    receiver.receivePromisedFilesAtDestination_options_operationQueue_reader(
                        &dir,
                        &NSDictionary::new(),
                        &self.ivars().queue,
                        &reader,
                    );
                }
            }
            true
        }

        /// The drop's point in the GPUI view's coordinates: points from its top left.
        fn location(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> (f64, f64) {
            // SAFETY: AppKit rule: the view hierarchy is read on the main thread, where this
            // view lives (`MainThreadOnly`) and AppKit delivers dragging messages.
            let Some(host) = (unsafe { self.superview() }) else { return (0.0, 0.0) };
            let at = host.convertPoint_fromView(sender.draggingLocation(), None);
            let y = if host.isFlipped() { at.y } else { host.bounds().size.height - at.y };
            (at.x, y)
        }
    }

    /// A view over the whole of `host`, beneath its web pages, that takes drops of promised
    /// files and nothing else.
    pub fn install(host: NonNull<c_void>, key: usize) -> Option<Retained<DropView>> {
        let mtm = MainThreadMarker::new()?;
        // SAFETY: `raw_window_handle`'s AppKit rule: the handle is a live `NSView` of the
        // window, valid while the window is; the app installs this once, for the window it
        // keeps for its whole run.
        let host: Retained<NSView> = unsafe { Retained::retain(host.as_ptr().cast::<NSView>()) }?;
        let queue = NSOperationQueue::new();
        queue.setMaxConcurrentOperationCount(1);
        let this = DropView::alloc(mtm).set_ivars(Ivars { host: key, queue });
        // SAFETY: `NSView`'s designated initialiser on a freshly allocated instance.
        let view: Retained<DropView> =
            unsafe { msg_send![super(this), initWithFrame: host.bounds()] };
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        view.registerForDraggedTypes(&NSFilePromiseReceiver::readableDraggedTypes());
        host.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, None);
        tracing::debug!("promised file drops accepted");
        Some(view)
    }
}

#[cfg(target_os = "ios")]
mod ios {
    use std::ffi::c_void;
    use std::path::{Path, PathBuf};
    use std::ptr::NonNull;
    use std::sync::Arc;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2::{DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_foundation::{NSArray, NSError, NSObject, NSString, NSURL};
    use objc2_ui_kit::{
        UIDragDropSession as _, UIDropInteraction, UIDropInteractionDelegate, UIDropOperation,
        UIDropProposal, UIDropSession, UIInteraction as _, UIView,
    };
    use parking_lot::Mutex;

    use super::{Landing, deliver, root};

    /// The type every file representation conforms to.
    const DATA_UTI: &str = "public.data";

    pub struct Ivars {
        host: usize,
    }

    define_class!(
        // SAFETY:
        // - `NSObject` has no subclassing requirements.
        // - `Delegate` does not implement `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SloptyDropDelegate"]
        #[ivars = Ivars]
        pub struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UIDropInteractionDelegate for Delegate {
            #[unsafe(method(dropInteraction:canHandleSession:))]
            fn can_handle(
                &self,
                _interaction: &UIDropInteraction,
                session: &ProtocolObject<dyn UIDropSession>,
            ) -> bool {
                let types = NSArray::from_retained_slice(&[NSString::from_str(DATA_UTI)]);
                session.hasItemsConformingToTypeIdentifiers(&types)
            }

            #[unsafe(method_id(dropInteraction:sessionDidUpdate:))]
            fn update(
                &self,
                _interaction: &UIDropInteraction,
                _session: &ProtocolObject<dyn UIDropSession>,
            ) -> Retained<UIDropProposal> {
                let mtm = self.mtm();
                UIDropProposal::initWithDropOperation(
                    UIDropProposal::alloc(mtm),
                    UIDropOperation::Copy,
                )
            }

            #[unsafe(method(dropInteraction:performDrop:))]
            fn perform(
                &self,
                interaction: &UIDropInteraction,
                session: &ProtocolObject<dyn UIDropSession>,
            ) {
                self.receive(interaction, session);
            }
        }
    );

    impl std::fmt::Debug for Delegate {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Delegate")
        }
    }

    /// `name` in `dir`, numbered when a file of the drop already took it.
    fn free_name(dir: &Path, name: &str) -> PathBuf {
        let first = dir.join(name);
        if !first.exists() {
            return first;
        }
        let (stem, ext) = match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
            _ => (name, String::new()),
        };
        (2..=u32::MAX)
            .map(|n| dir.join(format!("{stem} {n}{ext}")))
            .find(|p| !p.exists())
            .unwrap_or(first)
    }

    impl Delegate {
        /// Load each item's first file representation into a landing of their own.
        fn receive(
            &self,
            interaction: &UIDropInteraction,
            session: &ProtocolObject<dyn UIDropSession>,
        ) {
            let items = session.items();
            let at = interaction.view().map_or((0.0, 0.0), |view| {
                let p = session.locationInView(&view);
                (p.x, p.y)
            });
            let landing = match Landing::new(&root(), items.count(), at) {
                Ok(landing) => landing,
                Err(e) => {
                    tracing::warn!(error = %e, "no directory for a drop");
                    return;
                }
            };
            let dir = landing.dir().to_path_buf();
            tracing::info!(files = items.count(), dir = %dir.display(), "files dropped");
            let landing = Arc::new(Mutex::new(landing));
            let host = self.ivars().host;
            for item in &items {
                let provider = item.itemProvider();
                let suggested = provider.suggestedName().map(|n| n.to_string());
                let types = provider.registeredTypeIdentifiers();
                let (landing, dir) = (Arc::clone(&landing), dir.clone());
                let Some(uti) = types.iter().next() else {
                    let dropped = landing.lock().resolve(Err((None, "nothing to read".to_owned())));
                    if let Some(dropped) = dropped {
                        deliver(host, dropped);
                    }
                    continue;
                };
                let copied = RcBlock::new(move |url: *mut NSURL, error: *mut NSError| {
                    // SAFETY: Foundation rule: the URL is null or valid for the call; the
                    // file at it is removed once the call returns, so it is copied here.
                    let url = unsafe { url.as_ref() };
                    // SAFETY: Foundation rule: the error is null or valid for the call.
                    let error = unsafe { error.as_ref() };
                    let from = url.and_then(NSURL::path).map(|p| PathBuf::from(p.to_string()));
                    let outcome = match (from, error) {
                        (Some(from), None) => {
                            let name = from
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .or_else(|| suggested.clone())
                                .unwrap_or_else(|| "dropped".to_owned());
                            let to = free_name(&dir, &name);
                            match std::fs::copy(&from, &to) {
                                Ok(_bytes) => Ok(to),
                                Err(e) => Err((Some(to), e.to_string())),
                            }
                        }
                        (_from, Some(error)) => {
                            Err((None, error.localizedDescription().to_string()))
                        }
                        (None, None) => Err((None, "not a file".to_owned())),
                    };
                    let dropped = landing.lock().resolve(outcome);
                    if let Some(dropped) = dropped {
                        deliver(host, dropped);
                    }
                });
                // SAFETY: Foundation rule: a registered type identifier and a completion
                // called once, off the main thread, which it may be: it holds only `Send`
                // values. The progress may be ignored.
                let _progress = unsafe {
                    provider
                        .loadFileRepresentationForTypeIdentifier_completionHandler(&uti, &copied)
                };
            }
        }
    }

    /// A drop interaction on `host`, whose delegate lands every file of a drop.
    pub fn install(
        host: NonNull<c_void>,
        key: usize,
    ) -> Option<(Retained<Delegate>, Retained<UIDropInteraction>)> {
        let mtm = MainThreadMarker::new()?;
        // SAFETY: `raw_window_handle`'s UIKit rule: the handle is a live `UIView` of the
        // window, valid while the window is; the app installs this once, for the window it
        // keeps for its whole run.
        let host: Retained<UIView> = unsafe { Retained::retain(host.as_ptr().cast::<UIView>()) }?;
        let this = Delegate::alloc(mtm).set_ivars(Ivars { host: key });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
        let delegate: Retained<Delegate> = unsafe { msg_send![super(this), init] };
        let interaction = UIDropInteraction::initWithDelegate(
            UIDropInteraction::alloc(mtm),
            ProtocolObject::from_ref(&*delegate),
        );
        host.addInteraction(ProtocolObject::from_ref(&*interaction));
        tracing::debug!("file drops accepted");
        Some((delegate, interaction))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every drop gets a directory of its own; the files that arrived whole are handed on in
    /// order once the last one resolves, and one that failed is named, left out, and what it
    /// wrote is gone.
    #[test]
    fn a_drop_lands_in_its_own_directory_and_only_whole_files_go_on() {
        let tmp = tempfile::tempdir().unwrap();
        let mut landing = Landing::new(tmp.path(), 3, (40.0, 12.5)).unwrap();
        let other = Landing::new(tmp.path(), 1, (0.0, 0.0)).unwrap();
        assert_ne!(landing.dir(), other.dir(), "a directory per drop");
        assert!(landing.dir().is_dir() && landing.dir().starts_with(tmp.path()));

        let mail = landing.dir().join("Re: plans.eml");
        std::fs::write(&mail, b"From: a").unwrap();
        assert_eq!(landing.resolve(Ok(mail.clone())), None, "one of three");
        let partial = landing.dir().join("IMG_0001.heic");
        std::fs::write(&partial, b"half").unwrap();
        let cut = Err((Some(partial.clone()), "The photo is not downloaded".to_owned()));
        assert_eq!(landing.resolve(cut), None);
        assert!(!partial.exists(), "a partial file is not kept");
        let photo = landing.dir().join("IMG_0002.heic");
        std::fs::write(&photo, b"whole").unwrap();
        let dropped = landing.resolve(Ok(photo.clone())).unwrap();
        assert_eq!(dropped.paths, [mail, photo], "whole files, in the order they arrived");
        assert_eq!(dropped.failed, ["IMG_0001.heic: The photo is not downloaded"]);
        assert_eq!((dropped.x, dropped.y), (40.0, 12.5));
    }

    /// A failure that names no file, or a file outside the drop's directory, removes nothing.
    #[test]
    fn a_failure_never_removes_anything_outside_the_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let mut landing = Landing::new(&tmp.path().join("drops"), 2, (0.0, 0.0)).unwrap();
        let outside = tmp.path().join("mine.txt");
        std::fs::write(&outside, b"keep").unwrap();
        assert_eq!(landing.resolve(Err((Some(outside.clone()), "no".to_owned()))), None);
        assert!(outside.exists());
        let dropped = landing.resolve(Err((None, "cancelled".to_owned()))).unwrap();
        assert!(dropped.paths.is_empty());
        assert_eq!(dropped.failed, ["mine.txt: no", "A file: cancelled"]);
    }
}

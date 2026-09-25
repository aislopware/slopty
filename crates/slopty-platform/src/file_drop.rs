//! Files dropped from apps that promise them rather than name them.
//!
//! A Finder drop names files, and GPUI hands those to the tile under the pointer. Mail and
//! Photos drag file promises instead (`NSFilePromiseReceiver`): the file is written only once
//! the drop names a directory. On iPad every drop is item providers (`UIDropInteraction`).
//! Both land here in a fresh temporary directory ([`Landing`]). The files that arrived whole go
//! to the sink as one [`Dropped`], with where the drop was, for the app to hand to the tile
//! there as an ordinary file drop, which uploads them. A file that failed is reported by name
//! and left out, and whatever it wrote is deleted.
//!
//! A landing is deleted as soon as nothing in it is going anywhere: every file failed, or no
//! view took the drop. Otherwise it is [`Dropped::landing`], for whoever uploads the files to
//! [`discard`] once the upload ends. Landings a run of the app left behind (it quit mid-upload,
//! or crashed) are swept when the next one starts ([`sweep`]).

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
    /// The drop's own directory the files are in, to [`discard`] once they are uploaded;
    /// `None` when the drop named files where they already were.
    pub landing: Option<PathBuf>,
    /// The files that did not, each as `name: why`.
    pub failed: Vec<String>,
    /// Where the drop was, in the GPUI view's points from its top left.
    pub x: f64,
    /// See `x`.
    pub y: f64,
}

/// Where each drop's files are received: a directory of its own under the system's temporary
/// directory, named `<pid>-<n>` for the process that received it.
#[must_use]
pub fn root() -> PathBuf {
    std::env::temp_dir().join("slopty-drops")
}

/// Delete a drop's landing, files and all, once they are uploaded or will not be. Nothing
/// outside [`root`] is touched, whatever `landing` says.
pub fn discard(landing: &Path) {
    discard_in(&root(), landing);
}

fn discard_in(root: &Path, landing: &Path) {
    if landing.parent() != Some(root) {
        tracing::warn!(landing = %landing.display(), "not a drop's landing; kept");
        return;
    }
    if let Err(e) = std::fs::remove_dir_all(landing)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(landing = %landing.display(), error = %e, "remove a drop's landing");
    }
}

/// Delete the landings under `root` of processes that are gone: a run that quit before its
/// uploads ended, or crashed. Another running app's, and this one's, stay.
pub fn sweep(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid =
            name.to_str().and_then(|n| n.split_once('-')).and_then(|(pid, _)| pid.parse().ok());
        if let Some(pid) = pid.and_then(rustix::process::Pid::from_raw)
            && !alive(pid)
        {
            discard_in(root, &entry.path());
        }
    }
}

/// Whether a process with this id exists (signal 0 checks without sending anything; a process
/// of another user's answers "not permitted", which is still alive).
fn alive(pid: rustix::process::Pid) -> bool {
    !matches!(rustix::process::test_kill_process(pid), Err(rustix::io::Errno::SRCH))
}

/// `name` in `dir`, numbered `name 2`, `name 3`… when a file already took it.
///
/// It is created empty, so no other file of the drop can take it too: the iPad's files arrive
/// on threads of their own, at once, often under one name.
///
/// # Errors
///
/// When a file cannot be created in `dir` for another reason than the name being taken.
pub fn reserve(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    let candidates = std::iter::once(dir.join(name))
        .chain((2..=u32::MAX).map(|n| dir.join(format!("{stem} {n}{ext}"))));
    for path in candidates {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_file) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("every {name} is taken")))
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
        if self.resolved < self.expected {
            return None;
        }
        // Nothing arrived, so nothing will be uploaded from here.
        let landing = if self.paths.is_empty() {
            discard_in(self.dir.parent().unwrap_or(&self.dir), &self.dir);
            None
        } else {
            Some(self.dir.clone())
        };
        Some(Dropped {
            paths: std::mem::take(&mut self.paths),
            landing,
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
            if let Some(landing) = &dropped.landing {
                discard(landing);
            }
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
    let spawned =
        std::thread::Builder::new().name("slopty-drop-sweep".to_owned()).spawn(|| sweep(&root()));
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "sweep old drops");
    }
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
                    super::Dropped { paths: named, landing: None, failed: Vec::new(), x, y },
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
    use std::path::PathBuf;
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

    use super::{Landing, deliver, reserve, root};

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
                            match reserve(&dir, &name) {
                                Ok(to) => match std::fs::copy(&from, &to) {
                                    Ok(_bytes) => Ok(to),
                                    Err(e) => Err((Some(to), e.to_string())),
                                },
                                Err(e) => Err((None, format!("{name}: {e}"))),
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
        assert_eq!(dropped.landing.as_deref(), Some(landing.dir()), "kept for the upload");
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
        assert_eq!(dropped.landing, None, "nothing arrived, so nothing is uploaded from it");
        assert!(!landing.dir().exists(), "and its directory is gone at once");
    }

    /// Files of one drop arriving at once under one name each get a name of their own: none
    /// overwrites another.
    #[test]
    fn files_arriving_at_once_under_one_name_never_share_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let start = std::sync::Barrier::new(16);
        let mut names: Vec<PathBuf> = std::thread::scope(|s| {
            let mut racers = Vec::new();
            for _ in 0..16 {
                racers.push(s.spawn(|| {
                    start.wait();
                    reserve(dir, "IMG_0001.heic").unwrap()
                }));
            }
            racers.into_iter().map(|r| r.join().unwrap()).collect()
        });
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 16, "{names:?}");
        assert!(names.contains(&dir.join("IMG_0001.heic")));
        assert!(names.contains(&dir.join("IMG_0001 16.heic")));
        assert_eq!(reserve(dir, "notes").unwrap(), dir.join("notes"));
        assert_eq!(reserve(dir, "notes").unwrap(), dir.join("notes 2"));
        assert_eq!(reserve(dir, ".env").unwrap(), dir.join(".env"));
        assert_eq!(reserve(dir, ".env").unwrap(), dir.join(".env 2"));
        reserve(&dir.join("missing"), "a").unwrap_err();
    }

    /// A landing is deleted once it is done with, and nothing outside the drops' root ever is;
    /// the landings of a run that is gone are swept, a live one's are not.
    #[test]
    fn landings_are_discarded_and_a_dead_runs_are_swept() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drops");
        let landing = Landing::new(&root, 1, (0.0, 0.0)).unwrap();
        std::fs::write(landing.dir().join("a.txt"), b"a").unwrap();
        discard_in(&root, landing.dir());
        assert!(!landing.dir().exists());
        let elsewhere = tmp.path().join("mine");
        std::fs::create_dir_all(&elsewhere).unwrap();
        discard_in(&root, &elsewhere);
        assert!(elsewhere.exists(), "not a landing");

        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let gone = child.id();
        child.wait().unwrap();
        let dead = root.join(format!("{gone}-3"));
        std::fs::create_dir_all(dead.join("sub")).unwrap();
        let ours = Landing::new(&root, 1, (0.0, 0.0)).unwrap();
        let stray = root.join("not-a-pid");
        std::fs::create_dir_all(&stray).unwrap();
        sweep(&root);
        assert!(!dead.exists(), "a run that is gone");
        assert!(ours.dir().exists(), "this run's");
        assert!(stray.exists(), "not a landing's name");
        sweep(&tmp.path().join("none"));
    }
}

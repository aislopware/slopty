//! Drags and drops onto the app's window.
//!
//! On the Mac one view over the whole window takes every drag ([`DropSink`]), and asks the app
//! what is under it ([`Over`]). Over a remote window or display, the drag is the app's: it
//! carries what the drag holds to the worker, which drops it at the point there, and the badge
//! is what the worker says a drop would do. Anywhere else the drag goes on to GPUI's own
//! handling in the window, as if this view were not there, so a Finder drop names files that
//! GPUI hands to the tile under the pointer.
//!
//! Mail and Photos drag file promises instead (`NSFilePromiseReceiver`): the file is written
//! only once the drop names a directory, which macOS allows only inside the drop. On iPad every
//! drop is item providers (`UIDropInteraction`). Both land here in a fresh temporary directory
//! ([`Landing`]). The files that arrived whole go to the sink as one [`Dropped`], with where the
//! drop was, for the app to hand to the tile there as an ordinary file drop, which uploads
//! them, or to the remote drop that called them in. A file that failed is reported by name and
//! left out, and whatever it wrote is deleted.
//!
//! A landing is deleted as soon as nothing in it is going anywhere: every file failed, or no
//! view took the drop. Otherwise it is [`Dropped::landing`], for whoever uploads the files to
//! [`discard`] once the upload ends. Landings a run of the app left behind (it quit mid-upload,
//! or crashed) are swept when the next one starts ([`sweep`]).
//!
//! The other way on iOS, [`out`]: a worker's file dragged out of an iPad, or saved to Files. The
//! Files picker, both ways, is `picker` (iOS).

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod out;
#[cfg(target_os = "ios")]
pub mod picker;

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
    /// A drop over a remote tile called them in ([`DropSink::dropped`]), which they go to.
    pub remote: bool,
}

/// What a drag over the window is over, as the app decides from its point.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Over {
    /// The app's own: a local tile, the bars. The drag goes on to GPUI's handling.
    Local,
    /// A remote tile's picture: what a drop there would do, as the worker last said.
    Remote(slopty_proto::drag::DragOp),
}

/// What the app does with a drop over a remote tile.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Taken {
    /// Refused: it slides back.
    Refused,
    /// Taken, and the files it promises are called in.
    CallIn,
    /// Taken with everything it carries already where the drop needs it: nothing is called
    /// in, as for a drag this app began from the same worker.
    AsIs,
}

/// Where a host view's drags and drops go, on the main thread.
pub trait DropSink {
    /// A drag is at `at` (the view's points from its top left), carrying what is on `board`,
    /// which its source lets a target copy, link or move as `allowed` says: what it is over.
    /// `own` is the tag of a drag this app began ([`crate::drag::drag_out_items`]). The app
    /// follows the drag from one remote tile to another, and off them, itself.
    #[cfg(target_os = "macos")]
    fn over(
        &self,
        at: (f64, f64),
        board: &dyn crate::pasteboard::Pasteboard,
        allowed: slopty_proto::drag::DragOps,
        own: Option<u64>,
    ) -> Over;
    /// The drag left the window from over a remote tile, or ended there with no drop.
    #[cfg(target_os = "macos")]
    fn left(&self);
    /// Dropped at `at` over a remote tile, promising `promised` files: what the app does with
    /// it. Files called in arrive at [`Self::arrived`] marked [`Dropped::remote`].
    #[cfg(target_os = "macos")]
    fn dropped(&self, at: (f64, f64), promised: usize) -> Taken;
    /// Files a drop called in have arrived, or failed to.
    fn arrived(&self, dropped: Dropped);
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
    claim(dir, name, |path| {
        std::fs::OpenOptions::new().write(true).create_new(true).open(path).map(drop)
    })
}

/// [`reserve`] for a folder: the name is taken by an empty directory.
///
/// # Errors
///
/// As [`reserve`].
pub fn reserve_dir(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    #[expect(clippy::create_dir, reason = "an existing directory is another file's name")]
    claim(dir, name, |path| std::fs::create_dir(path))
}

/// The first of `name`, `name 2`, `name 3`… in `dir` that `make` creates.
fn claim(
    dir: &Path,
    name: &str,
    make: impl Fn(&Path) -> std::io::Result<()>,
) -> std::io::Result<PathBuf> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    let candidates = std::iter::once(dir.join(name))
        .chain((2..=u32::MAX).map(|n| dir.join(format!("{stem} {n}{ext}"))));
    for path in candidates {
        match make(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("every {name} is taken")))
}

/// Move the file or folder at `from` into `dir` as `name`, numbered when the name is taken.
///
/// What the system hands over for a drop or a pick is a temporary copy that is the app's to
/// take, so it is moved rather than copied; across volumes it is copied. The error names what
/// was written, for [`Landing::resolve`] to remove.
///
/// # Errors
///
/// When no name can be taken in `dir`, or the move and the copy both fail.
pub fn arrive(dir: &Path, from: &Path, name: &str) -> Result<PathBuf, (Option<PathBuf>, String)> {
    let folder = from.is_dir();
    let to = if folder { reserve_dir(dir, name) } else { reserve(dir, name) }
        .map_err(|e| (None, format!("{name}: {e}")))?;
    match std::fs::rename(from, &to) {
        Ok(()) => Ok(to),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => match copy_tree(from, &to) {
            Ok(()) => Ok(to),
            Err(e) => Err((Some(to), e.to_string())),
        },
        Err(e) => Err((Some(to), e.to_string())),
    }
}

/// Copy the file or folder at `from` to `to`, which may exist empty.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if !from.is_dir() {
        return std::fs::copy(from, to).map(drop);
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_tree(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

/// Remove what a file that failed wrote: a file, or a folder and all in it.
fn remove_partial(path: &Path) {
    let gone =
        if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    if let Err(e) = gone
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "remove a partial file");
    }
}

/// A fresh directory under `root` (made if missing), named `<pid>-<n>` for this process, so
/// [`sweep`] can tell a run that is gone.
///
/// # Errors
///
/// When `root` or the directory cannot be made.
pub fn fresh_dir(root: &Path) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(root)?;
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = root.join(format!("{}-{n}", std::process::id()));
        #[expect(clippy::create_dir, reason = "an existing directory is another drop's")]
        let made = std::fs::create_dir(&dir);
        match made {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
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
    remote: bool,
}

impl Landing {
    /// A fresh directory under `root` for a drop of `expected` files at `at`.
    ///
    /// # Errors
    ///
    /// As [`fresh_dir`].
    pub fn new(root: &Path, expected: usize, at: (f64, f64)) -> std::io::Result<Self> {
        let dir = fresh_dir(root)?;
        let (paths, failed) = (Vec::new(), Vec::new());
        Ok(Self { dir, expected, resolved: 0, paths, failed, at, remote: false })
    }

    /// The same, for the drop over a remote tile that called the files in.
    #[must_use]
    pub const fn for_remote(mut self) -> Self {
        self.remote = true;
        self
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
                    remove_partial(&partial);
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
            remote: self.remote,
        })
    }
}

/// Where a view's drags and drops go.
type Sink = Rc<dyn DropSink>;

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
            sink.arrived(dropped);
        } else {
            tracing::warn!(files = dropped.paths.len(), "a drop for a view that is gone");
            if let Some(landing) = &dropped.landing {
                discard(landing);
            }
        }
    });
}

/// The sink of the view `host`, while it has one.
#[cfg(target_os = "macos")]
fn sink_of(host: usize) -> Option<Sink> {
    SINKS.with(|s| s.borrow().iter().find(|(h, ..)| *h == host).map(|(_, s, _)| Rc::clone(s)))
}

/// Take the drags and drops on `host` and hand them to `sink` on the main thread (the module
/// docs).
///
/// `host` is the view a GPUI window draws into (its `raw_window_handle` handle). Once per view:
/// `false` off the main thread or when already done.
pub fn install(host: NonNull<c_void>, sink: Rc<dyn DropSink>) -> bool {
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
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::ptr::NonNull;
    use std::sync::Arc;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, NSObjectProtocol, ProtocolObject, Sel};
    use objc2::{
        ClassType as _, DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class,
        msg_send, sel,
    };
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSDragOperation, NSDraggingDestination, NSDraggingInfo,
        NSFilePromiseReceiver, NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypePDF,
        NSPasteboardTypeURL, NSResponder, NSView, NSWindowOrderingMode,
    };
    use objc2_foundation::{
        NSArray, NSDictionary, NSError, NSObject, NSOperationQueue, NSPoint, NSString, NSURL,
    };
    use parking_lot::Mutex;
    use slopty_proto::drag::{DragOp, DragOps};

    use super::{Landing, Over, deliver, root, sink_of};
    use crate::pasteboard::{MacPasteboard, uti_of};

    /// Whose the drag is, as it last went.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Route {
        /// Not over the window.
        Outside,
        /// GPUI's.
        Local,
        /// The app's, over a remote tile.
        Remote,
    }

    pub(super) struct Ivars {
        host: usize,
        /// Where the promised files are written from, off the main thread, one at a time.
        queue: Retained<NSOperationQueue>,
        route: Cell<Route>,
    }

    define_class!(
        // SAFETY:
        // - `NSView` may be subclassed; the overrides keep its contracts (a hit test that
        //   finds nothing, a dragging destination).
        // - `DropView` does not implement `Drop`.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SloptyDropView"]
        #[ivars = Ivars]
        pub(super) struct DropView;

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
            fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                self.track(sender)
            }

            #[unsafe(method(draggingUpdated:))]
            fn dragging_updated(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                self.track(sender)
            }

            #[unsafe(method(draggingExited:))]
            fn dragging_exited(&self, sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
                match self.ivars().route.replace(Route::Outside) {
                    Route::Remote => {
                        if let Some(sink) = sink_of(self.ivars().host) {
                            sink.left();
                        }
                    }
                    Route::Local => {
                        if let Some(sender) = sender {
                            self.window_hears(sel!(draggingExited:), sender);
                        }
                    }
                    Route::Outside => {}
                }
            }

            #[unsafe(method(performDragOperation:))]
            fn perform(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
                match self.ivars().route.get() {
                    Route::Remote => self.drop_remote(sender),
                    Route::Local | Route::Outside => self.drop_local(sender),
                }
            }

            #[unsafe(method(concludeDragOperation:))]
            fn conclude(&self, sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
                if self.ivars().route.replace(Route::Outside) == Route::Local
                    && let Some(sender) = sender
                {
                    self.window_hears(sel!(concludeDragOperation:), sender);
                }
            }
        }
    );

    impl std::fmt::Debug for DropView {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("DropView")
        }
    }

    /// What a drag's source lets a target do, as the wire says it. A generic operation is a
    /// copy, as AppKit's own targets take it.
    fn allowed(mask: NSDragOperation) -> DragOps {
        let mut ops = DragOps::empty();
        if mask.intersects(NSDragOperation::Copy | NSDragOperation::Generic) {
            ops |= DragOps::COPY;
        }
        if mask.contains(NSDragOperation::Link) {
            ops |= DragOps::LINK;
        }
        if mask.contains(NSDragOperation::Move) {
            ops |= DragOps::MOVE;
        }
        ops
    }

    /// What AppKit shows for `op`.
    const fn operation(op: DragOp) -> NSDragOperation {
        match op {
            DragOp::None => NSDragOperation::None,
            DragOp::Copy => NSDragOperation::Copy,
            DragOp::Link => NSDragOperation::Link,
            DragOp::Move => NSDragOperation::Move,
        }
    }

    /// The `file://` paths the drag names, one per item that names one.
    fn named(pasteboard: &NSPasteboard) -> Vec<PathBuf> {
        let Some(items) = pasteboard.pasteboardItems() else { return Vec::new() };
        // SAFETY: AppKit rule: the extern string constant is valid for the process's life.
        let url_type = unsafe { NSPasteboardTypeFileURL };
        items
            .iter()
            .filter_map(|item| NSURL::URLWithString(&*item.stringForType(url_type)?))
            .filter_map(|url| url.path().map(|p| PathBuf::from(p.to_string())))
            .collect()
    }

    /// The drag's file promises.
    fn receivers(pasteboard: &NSPasteboard) -> Vec<Retained<NSFilePromiseReceiver>> {
        let class: &AnyClass = NSFilePromiseReceiver::class();
        let classes = NSArray::from_slice(&[class]);
        // SAFETY: AppKit rule: an array of classes that adopt `NSPasteboardReading` and no
        // options; what comes back is instances of those classes.
        let objects = unsafe { pasteboard.readObjectsForClasses_options(&classes, None) };
        objects
            .map(|o| o.iter().filter_map(|o| o.downcast::<NSFilePromiseReceiver>().ok()).collect())
            .unwrap_or_default()
    }

    /// How many files `receivers` will write: as many as each names types, one at least.
    fn promised(receivers: &[Retained<NSFilePromiseReceiver>]) -> usize {
        receivers.iter().map(|r| r.fileTypes().count().max(1)).sum()
    }

    impl DropView {
        /// The drag entered or moved: the app's over a remote tile, else the window's.
        fn track(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            let ivars = self.ivars();
            let Some(sink) = sink_of(ivars.host) else {
                return NSDragOperation::None;
            };
            let at = self.location(sender);
            let board = MacPasteboard::of(sender.draggingPasteboard());
            let own = crate::drag::own_tag(sender);
            let over = sink.over(at, &board, allowed(sender.draggingSourceOperationMask()), own);
            let was = ivars.route.get();
            match over {
                Over::Remote(op) => {
                    if was == Route::Local {
                        self.window_hears(sel!(draggingExited:), sender);
                    }
                    ivars.route.set(Route::Remote);
                    operation(op)
                }
                Over::Local => {
                    let entering = was != Route::Local;
                    ivars.route.set(Route::Local);
                    let answer =
                        if entering { sel!(draggingEntered:) } else { sel!(draggingUpdated:) };
                    let op = self.window_answers(answer, sender);
                    // GPUI takes only named files; a promise this view calls in at the drop.
                    if op == NSDragOperation::None
                        && !receivers(&sender.draggingPasteboard()).is_empty()
                    {
                        NSDragOperation::Copy
                    } else {
                        op
                    }
                }
            }
        }

        /// The window, GPUI's dragging destination, hears that the drag left it
        /// (`draggingExited:`), or that its drop is done (`concludeDragOperation:`).
        fn window_hears(&self, selector: Sel, sender: &ProtocolObject<dyn NSDraggingInfo>) {
            let Some(window) = self.window().filter(|w| w.respondsToSelector(selector)) else {
                return;
            };
            if selector == sel!(draggingExited:) {
                // SAFETY: AppKit's `NSDraggingDestination` rule: `draggingExited:` takes the
                // dragging info and returns nothing; GPUI's window implements it.
                let () = unsafe { msg_send![&*window, draggingExited: sender] };
            } else {
                // SAFETY: as above, for `concludeDragOperation:`.
                let () = unsafe { msg_send![&*window, concludeDragOperation: sender] };
            }
        }

        /// The window's answer to `selector` for `sender`; none when it has no answer.
        fn window_answers(
            &self,
            selector: Sel,
            sender: &ProtocolObject<dyn NSDraggingInfo>,
        ) -> NSDragOperation {
            let Some(window) = self.window().filter(|w| w.respondsToSelector(selector)) else {
                return NSDragOperation::None;
            };
            if selector == sel!(draggingEntered:) {
                // SAFETY: AppKit's `NSDraggingDestination` rule: `draggingEntered:` takes the
                // dragging info and returns an `NSDragOperation`; GPUI's window implements it.
                unsafe { msg_send![&*window, draggingEntered: sender] }
            } else {
                // SAFETY: as above, for `draggingUpdated:`.
                unsafe { msg_send![&*window, draggingUpdated: sender] }
            }
        }

        /// A drop over a remote tile: the app takes it or refuses it, and its promises are
        /// called in for the drop once it is taken.
        fn drop_remote(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            self.ivars().route.set(Route::Outside);
            let Some(sink) = sink_of(self.ivars().host) else { return false };
            let pasteboard = sender.draggingPasteboard();
            // A drag that names its files goes with those names, as the app read them.
            let receivers =
                if named(&pasteboard).is_empty() { receivers(&pasteboard) } else { Vec::new() };
            let at = self.location(sender);
            let expected = promised(&receivers);
            match sink.dropped(at, expected) {
                super::Taken::Refused => return false,
                super::Taken::CallIn if expected > 0 => {
                    self.receive(&receivers, at, true);
                }
                super::Taken::CallIn | super::Taken::AsIs => {}
            }
            true
        }

        /// A drop anywhere else: GPUI takes named files; promises land here for the tile under
        /// the drop.
        fn drop_local(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let pasteboard = sender.draggingPasteboard();
            if !named(&pasteboard).is_empty() {
                let Some(window) = self.window() else { return false };
                // SAFETY: AppKit's `NSDraggingDestination` rule: `performDragOperation:` takes
                // the dragging info and returns a `BOOL`; GPUI's window implements it.
                return unsafe { msg_send![&*window, performDragOperation: sender] };
            }
            let receivers = receivers(&pasteboard);
            if receivers.is_empty() {
                return false;
            }
            self.receive(&receivers, self.location(sender), false)
        }

        /// Call `receivers`' files into a landing of their own: `false` when there is none.
        fn receive(
            &self,
            receivers: &[Retained<NSFilePromiseReceiver>],
            at: (f64, f64),
            remote: bool,
        ) -> bool {
            let expected = promised(receivers);
            let host = self.ivars().host;
            let landing = match Landing::new(&root(), expected, at) {
                Ok(landing) if remote => landing.for_remote(),
                Ok(landing) => landing,
                Err(e) => {
                    tracing::warn!(error = %e, "no directory for a drop");
                    if remote {
                        let failed = vec![format!("the dropped files: {e}")];
                        let (paths, (x, y)) = (Vec::new(), at);
                        deliver(
                            host,
                            super::Dropped { paths, landing: None, failed, x, y, remote },
                        );
                    }
                    return false;
                }
            };
            let Some(dir) = landing.dir().to_str().map(NSString::from_str) else { return false };
            let dir = NSURL::fileURLWithPath_isDirectory(&dir, true);
            tracing::info!(files = expected, dir = %landing.dir().display(), remote, "promised files dropped");
            let landing = Arc::new(Mutex::new(landing));
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

        /// The drag's point in the GPUI view's coordinates: points from its top left.
        fn location(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> (f64, f64) {
            // SAFETY: AppKit rule: the view hierarchy is read on the main thread, where this
            // view lives (`MainThreadOnly`) and AppKit delivers dragging messages.
            let Some(host) = (unsafe { self.superview() }) else { return (0.0, 0.0) };
            let at = host.convertPoint_fromView(sender.draggingLocation(), None);
            let y = if host.isFlipped() { at.y } else { host.bounds().size.height - at.y };
            (at.x, y)
        }
    }

    /// The types a drag over the window is taken for: files named or promised, and every
    /// format the clipboard carries.
    fn taken_types() -> Retained<NSArray<NSString>> {
        let mut types: Vec<Retained<NSString>> =
            NSFilePromiseReceiver::readableDraggedTypes().to_vec();
        types.extend(
            slopty_proto::transfer::ClipFormat::ALL
                .into_iter()
                .map(|f| NSString::from_str(uti_of(f))),
        );
        #[expect(deprecated, reason = "what GPUI's window registers, and older apps still drag")]
        // SAFETY: AppKit rule: the extern string constants are valid for the process's life.
        let more = unsafe {
            [objc2_app_kit::NSFilenamesPboardType, NSPasteboardTypeURL, NSPasteboardTypePDF]
        };
        types.extend(more.into_iter().map(objc2::Message::retain));
        NSArray::from_retained_slice(&types)
    }

    /// A view over the whole of `host`, beneath its web pages, that takes every drag over the
    /// window (the module docs).
    pub(super) fn install(host: NonNull<c_void>, key: usize) -> Option<Retained<DropView>> {
        let mtm = MainThreadMarker::new()?;
        // SAFETY: `raw_window_handle`'s AppKit rule: the handle is a live `NSView` of the
        // window, valid while the window is; the app installs this once, for the window it
        // keeps for its whole run.
        let host: Retained<NSView> = unsafe { Retained::retain(host.as_ptr().cast::<NSView>()) }?;
        let queue = NSOperationQueue::new();
        queue.setMaxConcurrentOperationCount(1);
        let ivars = Ivars { host: key, queue, route: Cell::new(Route::Outside) };
        let this = DropView::alloc(mtm).set_ivars(ivars);
        // SAFETY: `NSView`'s designated initialiser on a freshly allocated instance.
        let view: Retained<DropView> =
            unsafe { msg_send![super(this), initWithFrame: host.bounds()] };
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        view.registerForDraggedTypes(&taken_types());
        host.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, None);
        tracing::debug!("drags over the window taken");
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
    use objc2_foundation::{
        NSArray, NSError, NSItemProviderFileOptions, NSObject, NSString, NSURL,
    };
    use objc2_ui_kit::{
        UIDragDropSession as _, UIDropInteraction, UIDropInteractionDelegate, UIDropOperation,
        UIDropProposal, UIDropSession, UIInteraction as _, UIView,
    };
    use parking_lot::Mutex;

    use super::out::{DATA_UTI, FOLDER_UTI};
    use super::{Landing, arrive, deliver, root};

    pub(super) struct Ivars {
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
        pub(super) struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UIDropInteractionDelegate for Delegate {
            #[unsafe(method(dropInteraction:canHandleSession:))]
            fn can_handle(
                &self,
                _interaction: &UIDropInteraction,
                session: &ProtocolObject<dyn UIDropSession>,
            ) -> bool {
                let types = NSArray::from_retained_slice(&[
                    NSString::from_str(DATA_UTI),
                    NSString::from_str(FOLDER_UTI),
                ]);
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
        /// Load each item, a file or a folder, into a landing of their own.
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
                let (landing, dir) = (Arc::clone(&landing), dir.clone());
                // Loaded as a file or a folder, whatever its own type (a photo's HEIC, a PDF):
                // the provider brings the representation that conforms.
                let uti = [DATA_UTI, FOLDER_UTI].into_iter().map(NSString::from_str).find(|t| {
                    provider.hasRepresentationConformingToTypeIdentifier_fileOptions(
                        t,
                        NSItemProviderFileOptions::empty(),
                    )
                });
                let Some(uti) = uti else {
                    let dropped = landing.lock().resolve(Err((None, "nothing to read".to_owned())));
                    if let Some(dropped) = dropped {
                        deliver(host, dropped);
                    }
                    continue;
                };
                let copied = RcBlock::new(move |url: *mut NSURL, error: *mut NSError| {
                    // SAFETY: Foundation rule: the URL is null or valid for the call; the
                    // file at it is removed once the call returns, so it is moved out here.
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
                            arrive(&dir, &from, &name)
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
                // SAFETY: Foundation rule: a type some representation conforms to and a completion
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
    pub(super) fn install(
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

    /// What the system hands over, a file or a folder, is moved into the landing whole under a
    /// name of its own, and a second of one name is numbered rather than overwriting the first.
    #[test]
    fn a_dropped_file_or_folder_arrives_whole_under_a_name_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let landing = Landing::new(&tmp.path().join("drops"), 3, (0.0, 0.0)).unwrap();
        let given = tmp.path().join("given");
        std::fs::create_dir_all(given.join("src/deep")).unwrap();
        std::fs::write(given.join("src/deep/a.rs"), b"fn a() {}").unwrap();
        std::fs::write(given.join("notes.txt"), b"one").unwrap();
        let folder = arrive(landing.dir(), &given.join("src"), "src").unwrap();
        assert_eq!(folder, landing.dir().join("src"));
        assert_eq!(std::fs::read(folder.join("deep/a.rs")).unwrap(), b"fn a() {}");
        assert!(!given.join("src").exists(), "moved, not copied");
        let first = arrive(landing.dir(), &given.join("notes.txt"), "notes.txt").unwrap();
        std::fs::write(given.join("notes.txt"), b"two").unwrap();
        let second = arrive(landing.dir(), &given.join("notes.txt"), "notes.txt").unwrap();
        assert_eq!(second, landing.dir().join("notes 2.txt"));
        assert_eq!(std::fs::read(first).unwrap(), b"one");
        assert_eq!(std::fs::read(second).unwrap(), b"two");
        let (partial, _why) = arrive(landing.dir(), &given.join("gone"), "gone").unwrap_err();
        let mut failing = Landing::new(&tmp.path().join("drops"), 1, (0.0, 0.0)).unwrap();
        let moved_in = partial.map(|p| failing.dir().join(p.file_name().unwrap()));
        std::fs::create_dir_all(moved_in.clone().unwrap().join("half")).unwrap();
        failing.resolve(Err((moved_in.clone(), "cut".to_owned())));
        assert!(!moved_in.unwrap().exists(), "a partial folder is removed whole");
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

//! One `FSEvents` stream over a few paths, handing each event to a closure on a dispatch queue of
//! its own.
//!
//! Item-level events (`FileEvents`), delivered without waiting out the latency after a quiet
//! spell (`NoDefer`), and told when a watched root itself moves (`WatchRoot`). Two users: a
//! worktree's quick-open index (`find::watch`), which hears every path made or removed however
//! deep, and a folder tile's follower on macOS (`fswatch`), which hears a write inside a file,
//! which a kqueue watch on the folder cannot.
//!
//! `FSEventStreamStart` waits on fseventsd: 0.3 to 2.7 s a start on macOS 27, where a macOS
//! 26.6 runner made two in a 134 ms test. Meanwhile any other `FSEvents` call of the process
//! waits too, even `FSEventsGetCurrentEventId`. A caller that must not wait asks for a
//! [`Starting`], started on a dispatch queue, and is told when it is up.

use std::ffi::{CStr, OsStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;

use dispatch2::{DispatchQoS, DispatchQueue, DispatchRetained, GlobalQueueIdentifier};
use objc2_core_foundation::{CFArray, CFString};
use objc2_core_services::{
    ConstFSEventStreamRef, FSEventStreamContext, FSEventStreamCreate, FSEventStreamEventId,
    FSEventStreamGetLatestEventId, FSEventStreamInvalidate, FSEventStreamRef, FSEventStreamRelease,
    FSEventStreamSetDispatchQueue, FSEventStreamStart, FSEventStreamStop,
    FSEventsGetCurrentEventId, kFSEventStreamCreateFlagFileEvents, kFSEventStreamCreateFlagNoDefer,
    kFSEventStreamCreateFlagWatchRoot, kFSEventStreamEventIdSinceNow,
};
pub use objc2_core_services::{
    FSEventStreamEventFlags as Flags, kFSEventStreamEventFlagItemChangeOwner,
    kFSEventStreamEventFlagItemCloned, kFSEventStreamEventFlagItemCreated,
    kFSEventStreamEventFlagItemInodeMetaMod, kFSEventStreamEventFlagItemModified,
    kFSEventStreamEventFlagItemRemoved, kFSEventStreamEventFlagItemRenamed,
    kFSEventStreamEventFlagKernelDropped, kFSEventStreamEventFlagMustScanSubDirs,
    kFSEventStreamEventFlagRootChanged, kFSEventStreamEventFlagUnmount,
    kFSEventStreamEventFlagUserDropped,
};
use parking_lot::Mutex;

/// What a stream calls for each event: the item's path and what happened to it.
pub type Handler = dyn Fn(&Path, Flags) + Send + Sync;

/// Items made, removed or renamed: a change of the tree's paths.
pub const PATH_CHANGED: Flags = kFSEventStreamEventFlagItemCreated
    | kFSEventStreamEventFlagItemRemoved
    | kFSEventStreamEventFlagItemRenamed
    | kFSEventStreamEventFlagItemCloned;
/// Events lost, or the tree moved: what was heard no longer covers it.
pub const LOST: Flags = kFSEventStreamEventFlagMustScanSubDirs
    | kFSEventStreamEventFlagUserDropped
    | kFSEventStreamEventFlagKernelDropped
    | kFSEventStreamEventFlagRootChanged
    | kFSEventStreamEventFlagUnmount;

/// A running stream, stopped when dropped.
pub struct Stream {
    raw: FSEventStreamRef,
    queue: DispatchRetained<DispatchQueue>,
    /// The boxed handler the callback reads, the stream's until `drop` takes it back.
    handler: *mut Box<Handler>,
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fsevents::Stream")
    }
}

// SAFETY: the stream is only stopped, invalidated and released, once, from `drop`; FSEvents
// takes those calls from any thread (FSEvents.h: a stream scheduled on a dispatch queue is not
// tied to the thread that made it). The handler is `Send + Sync` and only the callback reads it.
unsafe impl Send for Stream {}
// SAFETY: nothing but `drop` touches the fields.
unsafe impl Sync for Stream {}

impl Stream {
    /// Start a stream over `paths` that gathers events for `latency` seconds before it calls
    /// `handler`, on a serial queue named `label`; `None` when `FSEvents` refuses it or a path
    /// is not UTF-8. It hears what happens from its start on, or every event after `after`
    /// when given, those before its start replayed from `FSEvents`' history first.
    pub fn start(
        paths: &[&Path],
        after: Option<FSEventStreamEventId>,
        latency: f64,
        label: &str,
        handler: Box<Handler>,
    ) -> Option<Self> {
        let names: Option<Vec<_>> =
            paths.iter().map(|p| p.to_str().map(CFString::from_str)).collect();
        let names = CFArray::from_retained_objects(&names?);
        let handler = Box::into_raw(Box::new(handler));
        let mut context = FSEventStreamContext {
            version: 0,
            info: handler.cast::<c_void>(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let flags = kFSEventStreamCreateFlagFileEvents
            | kFSEventStreamCreateFlagNoDefer
            | kFSEventStreamCreateFlagWatchRoot;
        // SAFETY: `FSEventStreamCreate` (CoreServices/FSEvents.h) copies the context and the
        // paths; `callback` reads `info`, which stays alive until `drop` frees it after the
        // stream is invalidated.
        let stream = unsafe {
            FSEventStreamCreate(
                None,
                Some(callback),
                &raw mut context,
                names.as_opaque(),
                after.unwrap_or(kFSEventStreamEventIdSinceNow),
                latency,
                flags,
            )
        };
        if stream.is_null() {
            // SAFETY: no stream holds the handler, so this is its only owner.
            drop(unsafe { Box::from_raw(handler) });
            return None;
        }
        let queue = DispatchQueue::new(label, None);
        // SAFETY: a stream made above, scheduled on a serial queue before it starts
        // (FSEvents.h).
        unsafe {
            FSEventStreamSetDispatchQueue(stream, Some(&queue));
        }
        // SAFETY: as above, now scheduled.
        let started = unsafe { FSEventStreamStart(stream) };
        // Dropped unstarted, it is invalidated and released like a running one.
        let running = Self { raw: stream, queue, handler };
        started.then_some(running)
    }
}

/// A stream started off the caller's thread, which says when it is up. It hears every event
/// after the last one the stream it takes over from had heard, or what happens from the moment
/// its start begins when it takes over from none, so the two leave no gap between them. Dropping
/// it stops the stream, up or not, and one dropped before its start began is never started.
#[derive(Debug)]
pub struct Starting {
    /// The stream once up, which lives as long as this does.
    stream: Arc<Mutex<Option<Stream>>>,
}

/// The stream a [`Starting`] asks for, as [`Stream::start`] takes it.
pub struct Wanted {
    /// The paths it watches.
    pub paths: Vec<PathBuf>,
    /// Seconds it gathers events for before it calls.
    pub latency: f64,
    /// Its queue's name.
    pub label: &'static str,
    /// What it calls for each event.
    pub handler: Box<Handler>,
}

impl Starting {
    /// Ask for the stream `wanted`, taking over from `after`, started on a global dispatch
    /// queue, which then calls `up` unless the stream was dropped first. A stream `FSEvents`
    /// refuses is said in the log, and `up` is not called.
    pub fn spawn(wanted: Wanted, after: Option<&Self>, up: Box<dyn FnOnce() + Send>) -> Self {
        let queue = GlobalQueueIdentifier::QualityOfService(DispatchQoS::UserInitiated);
        Self::spawn_on(&DispatchQueue::global_queue(queue), wanted, after, up)
    }

    /// [`Starting::spawn`] on `queue`.
    fn spawn_on(
        queue: &DispatchQueue,
        wanted: Wanted,
        after: Option<&Self>,
        up: Box<dyn FnOnce() + Send>,
    ) -> Self {
        let Wanted { paths, latency, label, handler } = wanted;
        let slot = Arc::new(Mutex::new(None));
        let kept = Arc::downgrade(&slot);
        let before = after.map(|b| Arc::downgrade(&b.stream));
        queue.exec_async(move || {
            if kept.strong_count() == 0 {
                return;
            }
            // Read here, off the caller's thread: both calls wait for any other stream of the
            // process starting.
            let heard = before
                .and_then(|b| b.upgrade())
                .and_then(|b| b.lock().as_ref().map(Stream::latest));
            // SAFETY: `FSEventsGetCurrentEventId` takes nothing and returns an id (FSEvents.h).
            let after = heard.unwrap_or_else(|| unsafe { FSEventsGetCurrentEventId() });
            let roots: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
            let Some(stream) = Stream::start(&roots, Some(after), latency, label, handler) else {
                tracing::info!(label, "no FSEvents stream");
                return;
            };
            // Dropped meanwhile, the stream is stopped here as it goes out of scope.
            let Some(slot) = kept.upgrade() else { return };
            *slot.lock() = Some(stream);
            drop(slot);
            up();
        });
        Self { stream: slot }
    }
}

impl Stream {
    /// The id of the last event this stream handed its handler, or the one it was started
    /// after while none has come (FSEvents.h, `FSEventStreamGetLatestEventId`).
    fn latest(&self) -> FSEventStreamEventId {
        // SAFETY: a stream made in `start` and not yet released: only `drop` does that.
        unsafe { FSEventStreamGetLatestEventId(self.raw) }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: the stream made in `start`, stopped once (FSEvents.h).
        unsafe {
            FSEventStreamStop(self.raw);
        }
        // SAFETY: then invalidated once: the callback is not called after it (FSEvents.h).
        unsafe {
            FSEventStreamInvalidate(self.raw);
        }
        // A callback already running on the queue ends before the handler goes.
        self.queue.exec_sync(|| {});
        // SAFETY: as above; nothing uses the stream after this.
        unsafe {
            FSEventStreamRelease(self.raw);
        }
        // SAFETY: the box leaked in `start`; the stream that read it is gone.
        drop(unsafe { Box::from_raw(self.handler) });
    }
}

/// The stream's callback: each event's path and flags to the handler.
unsafe extern "C-unwind" fn callback(
    _stream: ConstFSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: NonNull<c_void>,
    flags: NonNull<Flags>,
    _ids: NonNull<FSEventStreamEventId>,
) {
    // SAFETY: `info` is the boxed handler `start` gave the stream, alive until `drop`.
    let handler = unsafe { &*info.cast_const().cast::<Box<Handler>>() };
    // SAFETY: without `kFSEventStreamCreateFlagUseCFTypes`, `paths` is `count` C strings
    // (FSEvents.h, `FSEventStreamCallback`).
    let paths =
        unsafe { std::slice::from_raw_parts(paths.cast::<*const c_char>().as_ptr(), count) };
    // SAFETY: and `flags` is `count` flags, one for each path.
    let flags = unsafe { std::slice::from_raw_parts(flags.as_ptr(), count) };
    for (path, flag) in paths.iter().zip(flags) {
        if path.is_null() {
            continue;
        }
        // SAFETY: a NUL-terminated path FSEvents owns for the length of the call.
        let bytes = unsafe { CStr::from_ptr(*path) }.to_bytes();
        handler(Path::new(OsStr::from_bytes(bytes)), *flag);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use dispatch2::DispatchQueueAttr;

    use super::*;

    /// Longer than fseventsd takes to start a stream on a loaded machine (2.7 s seen).
    const UP: Duration = Duration::from_secs(10);

    /// Says when the handler that holds it is dropped.
    struct Released(mpsc::SyncSender<()>);

    impl Drop for Released {
        fn drop(&mut self) {
            let _sent = self.0.try_send(());
        }
    }

    #[test]
    fn a_stream_says_when_it_is_up_and_hears_what_happens_then() {
        let root = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(root.path()).unwrap();
        let (heard, events) = mpsc::channel();
        let (said, up) = mpsc::sync_channel(1);
        let asked = Instant::now();
        let _starting = Starting::spawn(
            Wanted {
                paths: vec![dir.clone()],
                latency: 0.01,
                label: "io.slopty.fsevents.test",
                handler: Box::new(move |path: &Path, flags: Flags| {
                    let _sent = heard.send((path.to_path_buf(), flags));
                }),
            },
            None,
            Box::new(move || {
                let _sent = said.try_send(());
            }),
        );
        up.recv_timeout(UP).unwrap();
        println!("asked → up: {:?}", asked.elapsed());
        let file = dir.join("made.txt");
        std::fs::write(&file, "x").unwrap();
        let made = std::iter::from_fn(|| events.recv_timeout(UP).ok())
            .find(|(path, flags)| *path == file && flags & kFSEventStreamEventFlagItemCreated != 0);
        assert!(made.is_some(), "the file made once the stream was up is heard");
    }

    /// A stream that takes over from another hears what happened after the last event that
    /// one heard, though it happened before its own start: here a file made in a folder only
    /// it watches, after the stream before (over a quiet folder) was up and before it began.
    #[test]
    fn a_stream_taking_over_hears_what_happened_since_the_last_one_heard() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let (quiet, busy) = (root.join("quiet"), root.join("busy"));
        for dir in [&quiet, &busy] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let (said, up) = mpsc::sync_channel(2);
        let first_up = said.clone();
        let before = Starting::spawn(
            Wanted {
                paths: vec![quiet],
                latency: 0.01,
                label: "io.slopty.fsevents.test.before",
                handler: Box::new(|_: &Path, _: Flags| {}),
            },
            None,
            Box::new(move || {
                let _sent = first_up.try_send(());
            }),
        );
        up.recv_timeout(UP).unwrap();
        let file = busy.join("made.txt");
        std::fs::write(&file, "x").unwrap();
        let (heard, events) = mpsc::channel();
        let _after = Starting::spawn(
            Wanted {
                paths: vec![busy],
                latency: 0.01,
                label: "io.slopty.fsevents.test.after",
                handler: Box::new(move |path: &Path, flags: Flags| {
                    let _sent = heard.send((path.to_path_buf(), flags));
                }),
            },
            Some(&before),
            Box::new(move || {
                let _sent = said.try_send(());
            }),
        );
        up.recv_timeout(UP).unwrap();
        let made = std::iter::from_fn(|| events.recv_timeout(UP).ok())
            .find(|(path, flags)| *path == file && flags & kFSEventStreamEventFlagItemCreated != 0);
        assert!(made.is_some(), "the file made before this stream began is heard");
    }

    #[test]
    fn a_stream_dropped_before_it_is_up_is_stopped_and_never_says_up() {
        let root = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(root.path()).unwrap();
        let (tx, released) = mpsc::sync_channel(1);
        let guard = Released(tx);
        let (said, up) = mpsc::sync_channel(1);
        // The queue is held until the drop, so the start always comes after it.
        let queue = DispatchQueue::new("io.slopty.fsevents.test.held", DispatchQueueAttr::SERIAL);
        let (open, opened) = mpsc::sync_channel::<()>(1);
        queue.exec_async(move || {
            let _open = opened.recv_timeout(UP);
        });
        let starting = Starting::spawn_on(
            &queue,
            Wanted {
                paths: vec![dir],
                latency: 0.01,
                label: "io.slopty.fsevents.test",
                handler: Box::new(move |_: &Path, _: Flags| {
                    let _held = &guard;
                }),
            },
            None,
            Box::new(move || {
                let _sent = said.try_send(());
            }),
        );
        drop(starting);
        open.send(()).unwrap();
        assert!(released.recv_timeout(UP).is_ok(), "the handler, and the stream with it, went");
        assert!(up.try_recv().is_err(), "a stream nobody waits for is not said to be up");
    }
}

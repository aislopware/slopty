//! One `FSEvents` stream over a few paths, handing each event to a closure on a dispatch queue of
//! its own.
//!
//! Item-level events (`FileEvents`), delivered without waiting out the latency after a quiet
//! spell (`NoDefer`), and told when a watched root itself moves (`WatchRoot`). Two users: a
//! worktree's quick-open index (`find::watch`), which hears every path made or removed however
//! deep, and a folder tile's follower on macOS (`fswatch`), which hears a write inside a file,
//! which a kqueue watch on the folder cannot.

use std::ffi::{CStr, OsStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr::NonNull;

use dispatch2::{DispatchQueue, DispatchRetained};
use objc2_core_foundation::{CFArray, CFString};
use objc2_core_services::{
    ConstFSEventStreamRef, FSEventStreamContext, FSEventStreamCreate, FSEventStreamEventId,
    FSEventStreamInvalidate, FSEventStreamRef, FSEventStreamRelease, FSEventStreamSetDispatchQueue,
    FSEventStreamStart, FSEventStreamStop, kFSEventStreamCreateFlagFileEvents,
    kFSEventStreamCreateFlagNoDefer, kFSEventStreamCreateFlagWatchRoot,
    kFSEventStreamEventIdSinceNow,
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
    /// is not UTF-8.
    pub fn start(
        paths: &[&Path],
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
                kFSEventStreamEventIdSinceNow,
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

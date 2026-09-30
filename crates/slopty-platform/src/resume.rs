//! What tells the app its links may have died under it: the Mac woke, its screens woke, its
//! session came back or was unlocked, the app came to the front, or the network path changed.
//!
//! Each is a [`Resume`] handed to a [`Sink`] from whatever thread saw it. The app probes every
//! link at once on one rather than waiting for silence to say a link is dead, which takes
//! seconds (`docs/decisions/transport.md`, "A resume probes every link at once"). A [`Source`]
//! is where resumes come from: the system's ([`System`]), or a test's ([`Injected`]), so no test
//! ever sleeps the Mac or changes its network.
//!
//! On Apple systems the sources are AppKit's and UIKit's notifications and Network.framework's
//! path monitor. Linux has no source yet: its seam is here (logind's `PrepareForSleep` and a
//! netlink route watch), and its [`System`] watches nothing.

use std::any::Any;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

/// Something after which a link may be dead, or on another path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Resume {
    /// The Mac woke from sleep.
    Woke,
    /// The displays woke: the lid opened on a Mac that kept running, or display sleep ended.
    ScreensWoke,
    /// The login session became active again (fast user switching back).
    SessionActive,
    /// The screen was unlocked.
    Unlocked,
    /// The app came to the front; on iOS, its scene came to the foreground.
    Foreground,
    /// The network path changed: another interface, another gateway, or it came up or went.
    PathChanged,
}

impl Resume {
    /// Every kind, for a test's sweep and the self-test's names.
    pub const ALL: [Self; 6] = [
        Self::Woke,
        Self::ScreensWoke,
        Self::SessionActive,
        Self::Unlocked,
        Self::Foreground,
        Self::PathChanged,
    ];

    /// The device was away (asleep, locked, another session): every link is in doubt from the
    /// moment it is back, and says so before a probe answers.
    #[must_use]
    pub const fn was_away(self) -> bool {
        matches!(self, Self::Woke | Self::ScreensWoke | Self::SessionActive | Self::Unlocked)
    }

    /// The path moved under the links: a connection migrates to it before it is probed.
    #[must_use]
    pub const fn moved(self) -> bool {
        matches!(self, Self::PathChanged)
    }

    /// Its name in logs and the self-test's commands.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Woke => "woke",
            Self::ScreensWoke => "screens-woke",
            Self::SessionActive => "session-active",
            Self::Unlocked => "unlocked",
            Self::Foreground => "foreground",
            Self::PathChanged => "path-changed",
        }
    }

    /// The kind [`Self::name`] names.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.name() == name)
    }
}

/// Where resumes go, from whatever thread saw them.
pub type Sink = Arc<dyn Fn(Resume) + Send + Sync>;

/// A running watch: resumes reach its sink until it is dropped.
pub struct Watch {
    _held: Box<dyn Any>,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch").finish_non_exhaustive()
    }
}

/// Where resumes come from.
pub trait Source {
    /// Hand every resume to `sink` from now until the watch is dropped.
    fn watch(&self, sink: Sink) -> Watch;
}

/// The system's resumes. Watched on the main thread, which the notifications are posted on.
#[derive(Clone, Copy, Debug, Default)]
pub struct System;

impl Source for System {
    fn watch(&self, sink: Sink) -> Watch {
        #[cfg(target_vendor = "apple")]
        {
            Watch { _held: apple::watch(&sink) }
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            drop(sink);
            Watch { _held: Box::new(()) }
        }
    }
}

/// Resumes a test fires by hand ([`Self::fire`]), to whichever sink watches it.
#[derive(Clone, Debug, Default)]
pub struct Injected {
    sink: Rc<RefCell<Option<Weak<Sink>>>>,
}

impl Injected {
    /// Hand `resume` to the sink watching, if one is.
    pub fn fire(&self, resume: Resume) {
        let sink = self.sink.borrow().as_ref().and_then(Weak::upgrade);
        if let Some(sink) = sink {
            sink(resume);
        }
    }
}

impl Source for Injected {
    fn watch(&self, sink: Sink) -> Watch {
        let held = Rc::new(sink);
        self.sink.replace(Some(Rc::downgrade(&held)));
        Watch { _held: Box::new(held) }
    }
}

/// A network path as far as a link cares: whether it is up, the interfaces it may use in the
/// system's order of preference, and its gateways.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathShape {
    /// `nw_path_status_t`: satisfied, unsatisfied, satisfiable.
    pub status: i32,
    /// The interfaces' names, the preferred first (`en0`, `en7`, `utun4`).
    pub interfaces: Vec<String>,
    /// The gateways' addresses.
    pub gateways: Vec<String>,
}

/// The paths seen so far, telling a change from the monitor saying the same path again (it
/// does, for a DNS server or a constrained flag that moves nothing a link rides on).
#[derive(Debug, Default)]
pub struct PathLog {
    last: Option<PathShape>,
}

impl PathLog {
    /// Take the path the monitor reports now. Whether it is a change: the first is the path
    /// the links were made on, not a change.
    pub fn changed(&mut self, now: PathShape) -> bool {
        let changed = self.last.as_ref().is_some_and(|last| *last != now);
        self.last = Some(now);
        changed
    }
}

#[cfg(target_vendor = "apple")]
mod apple {
    use std::ffi::{CStr, c_char, c_int};
    use std::ptr::NonNull;
    use std::sync::Arc;

    use block2::RcBlock;
    use dispatch2::{DispatchQueue, DispatchRetained};
    use objc2::Message as _;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, NSObjectProtocol, ProtocolObject};
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSNotificationName};
    use parking_lot::Mutex;

    use super::{PathLog, PathShape, Resume, Sink};

    // Network.framework (Network/path_monitor.h, path.h, interface.h, endpoint.h). Its objects
    // are OS objects, which under Objective-C are `NSObject`s: passed as `AnyObject` pointers
    // and owned through `Retained`. An enumeration block returns C `bool`, which is `BOOL`'s
    // representation on Apple silicon, the only architecture Slopty builds for.
    #[link(name = "Network", kind = "framework")]
    unsafe extern "C" {
        fn nw_path_monitor_create() -> *mut AnyObject;
        fn nw_path_monitor_set_queue(monitor: NonNull<AnyObject>, queue: &DispatchQueue);
        fn nw_path_monitor_set_update_handler(
            monitor: NonNull<AnyObject>,
            handler: &block2::Block<dyn Fn(NonNull<AnyObject>)>,
        );
        fn nw_path_monitor_start(monitor: NonNull<AnyObject>);
        fn nw_path_monitor_cancel(monitor: NonNull<AnyObject>);
        fn nw_path_get_status(path: NonNull<AnyObject>) -> c_int;
        fn nw_path_enumerate_interfaces(
            path: NonNull<AnyObject>,
            each: &block2::Block<dyn Fn(NonNull<AnyObject>) -> Bool>,
        );
        fn nw_interface_get_name(interface: NonNull<AnyObject>) -> *const c_char;
        fn nw_path_enumerate_gateways(
            path: NonNull<AnyObject>,
            each: &block2::Block<dyn Fn(NonNull<AnyObject>) -> Bool>,
        );
        fn nw_endpoint_copy_address_string(endpoint: NonNull<AnyObject>) -> *mut c_char;
    }

    /// An observer and the centre it was added to.
    type Observer =
        (Retained<NSNotificationCenter>, Retained<ProtocolObject<dyn NSObjectProtocol>>);
    /// A notification watched: its centre, its name, and the resume it is.
    type Watched = (Retained<NSNotificationCenter>, Retained<NSNotificationName>, Resume);

    /// Everything a watch holds: its observers, and the path monitor.
    struct Held {
        observers: Vec<Observer>,
        monitor: Option<PathMonitor>,
    }

    impl Drop for Held {
        fn drop(&mut self) {
            for (center, observer) in self.observers.drain(..) {
                // SAFETY: an observer `addObserverForName:…` returned on this centre, removed
                // once.
                unsafe {
                    center.removeObserver(observer.as_ref());
                }
            }
            drop(self.monitor.take());
        }
    }

    pub(super) fn watch(sink: &Sink) -> Box<dyn std::any::Any> {
        let mut observers = Vec::new();
        for (center, name, resume) in notifications() {
            let sink = Arc::clone(sink);
            let block = RcBlock::new(move |_note: NonNull<NSNotification>| sink(resume));
            // SAFETY: with no queue the block runs on the thread that posts; everything it
            // holds is `Send + Sync`, and the observer is removed before the block goes
            // (`Held::drop`).
            let observer = unsafe {
                center.addObserverForName_object_queue_usingBlock(Some(&name), None, None, &block)
            };
            observers.push((center, observer));
        }
        Box::new(Held { observers, monitor: PathMonitor::start(Arc::clone(sink)) })
    }

    /// Each notification watched: its centre, its name, and the resume it is.
    #[cfg(target_os = "macos")]
    fn notifications() -> Vec<Watched> {
        use objc2_app_kit::{
            NSApplicationDidBecomeActiveNotification, NSWorkspace, NSWorkspaceDidWakeNotification,
            NSWorkspaceScreensDidWakeNotification, NSWorkspaceSessionDidBecomeActiveNotification,
        };
        use objc2_foundation::NSDistributedNotificationCenter;
        let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
        let app = NSNotificationCenter::defaultCenter();
        let distributed = Retained::into_super(NSDistributedNotificationCenter::defaultCenter());
        // SAFETY: immutable `NSString` statics AppKit defines (NSWorkspace.h, NSApplication.h).
        let (woke, screens, session, active) = unsafe {
            (
                NSWorkspaceDidWakeNotification,
                NSWorkspaceScreensDidWakeNotification,
                NSWorkspaceSessionDidBecomeActiveNotification,
                NSApplicationDidBecomeActiveNotification,
            )
        };
        // The screen saver's and the login window's unlock, posted to every process through
        // the distributed centre; no header names it.
        let unlocked = objc2_foundation::NSString::from_str("com.apple.screenIsUnlocked");
        vec![
            (workspace.clone(), woke.retain(), Resume::Woke),
            (workspace.clone(), screens.retain(), Resume::ScreensWoke),
            (workspace, session.retain(), Resume::SessionActive),
            (app, active.retain(), Resume::Foreground),
            (distributed, unlocked, Resume::Unlocked),
        ]
    }

    /// Each notification watched: its centre, its name, and the resume it is.
    #[cfg(target_os = "ios")]
    fn notifications() -> Vec<Watched> {
        use objc2_ui_kit::{
            UIApplicationDidBecomeActiveNotification, UIApplicationProtectedDataDidBecomeAvailable,
            UISceneWillEnterForegroundNotification,
        };
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: immutable `NSString` statics UIKit defines (UIScene.h, UIApplication.h).
        let (scene, active, unlocked) = unsafe {
            (
                UISceneWillEnterForegroundNotification,
                UIApplicationDidBecomeActiveNotification,
                UIApplicationProtectedDataDidBecomeAvailable,
            )
        };
        vec![
            (center.clone(), scene.retain(), Resume::Foreground),
            (center.clone(), active.retain(), Resume::Foreground),
            (center, unlocked.retain(), Resume::Unlocked),
        ]
    }

    /// Network.framework's path monitor, on a queue of its own.
    struct PathMonitor {
        monitor: Retained<AnyObject>,
        _queue: DispatchRetained<DispatchQueue>,
    }

    impl PathMonitor {
        fn start(sink: Sink) -> Option<Self> {
            // SAFETY: no precondition (path_monitor.h).
            let created = unsafe { nw_path_monitor_create() };
            // SAFETY: `nw_path_monitor_create` returns a new monitor at +1 (path_monitor.h),
            // which `Retained` takes over.
            let monitor: Retained<AnyObject> = unsafe { Retained::from_raw(created) }?;
            let queue = DispatchQueue::new("app.slopty.path", None);
            let log = Mutex::new(PathLog::default());
            let handler = RcBlock::new(move |path: NonNull<AnyObject>| {
                // SAFETY: the monitor hands the handler a path valid for the call.
                let shape = unsafe { shape(path) };
                if log.lock().changed(shape.clone()) {
                    tracing::info!(?shape, "network path changed");
                    sink(Resume::PathChanged);
                }
            });
            let raw = NonNull::from(&*monitor);
            // SAFETY: a monitor not yet started takes its queue (path_monitor.h: set before
            // `nw_path_monitor_start`).
            unsafe {
                nw_path_monitor_set_queue(raw, &queue);
            }
            // SAFETY: as the queue; the handler is copied by the call, runs only on `queue`,
            // and holds only what is `Send`.
            unsafe {
                nw_path_monitor_set_update_handler(raw, &handler);
            }
            // SAFETY: queue and handler are set.
            unsafe {
                nw_path_monitor_start(raw);
            }
            Some(Self { monitor, _queue: queue })
        }
    }

    impl Drop for PathMonitor {
        fn drop(&mut self) {
            // SAFETY: cancelling a started monitor stops its handler (path_monitor.h); the
            // queue outlives the cancel, since this holds it.
            unsafe {
                nw_path_monitor_cancel(NonNull::from(&*self.monitor));
            }
        }
    }

    /// What of `path` a link rides on.
    ///
    /// # Safety
    ///
    /// `path` is a live `nw_path_t`.
    unsafe fn shape(path: NonNull<AnyObject>) -> PathShape {
        let interfaces = Arc::new(Mutex::new(Vec::new()));
        let gateways = Arc::new(Mutex::new(Vec::new()));
        let found = Arc::clone(&interfaces);
        let each_interface = RcBlock::new(move |interface: NonNull<AnyObject>| {
            // SAFETY: the enumeration hands a live interface; its name is a C string it owns
            // for the interface's life (interface.h).
            let name = unsafe { nw_interface_get_name(interface) };
            if !name.is_null() {
                // SAFETY: non-null, NUL-terminated, as above.
                let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
                found.lock().push(name);
            }
            Bool::YES
        });
        let found = Arc::clone(&gateways);
        let each_gateway = RcBlock::new(move |gateway: NonNull<AnyObject>| {
            // SAFETY: the enumeration hands a live endpoint; the copy is the caller's to free
            // (endpoint.h: "must be freed with free()").
            let address = unsafe { nw_endpoint_copy_address_string(gateway) };
            if !address.is_null() {
                // SAFETY: non-null and NUL-terminated, freed once below.
                let text = unsafe { CStr::from_ptr(address) }.to_string_lossy().into_owned();
                // SAFETY: allocated by `malloc` inside Network.framework, as above.
                unsafe {
                    libc::free(address.cast());
                }
                found.lock().push(text);
            }
            Bool::YES
        });
        // SAFETY: `path` is live for this call (the caller's contract); the enumeration runs
        // its block synchronously before returning (path.h).
        unsafe {
            nw_path_enumerate_interfaces(path, &each_interface);
        }
        // SAFETY: as the interfaces.
        unsafe {
            nw_path_enumerate_gateways(path, &each_gateway);
        }
        // SAFETY: `path` is live, as above.
        let status = unsafe { nw_path_get_status(path) };
        let interfaces = std::mem::take(&mut *interfaces.lock());
        let gateways = std::mem::take(&mut *gateways.lock());
        PathShape { status, interfaces, gateways }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A path is a change only against the one before: the first is where the links were made,
    /// and the monitor saying the same path again is no change.
    #[test]
    fn a_path_changes_only_when_what_a_link_rides_on_does() {
        let wifi = PathShape {
            status: 1,
            interfaces: vec!["en0".into()],
            gateways: vec!["192.168.1.1".into()],
        };
        let wired = PathShape { interfaces: vec!["en7".into(), "en0".into()], ..wifi.clone() };
        let other_router = PathShape { gateways: vec!["10.0.0.1".into()], ..wifi.clone() };
        let down = PathShape { status: 2, ..PathShape::default() };
        let mut log = PathLog::default();
        assert!(!log.changed(wifi.clone()), "the first path is the links' own");
        assert!(!log.changed(wifi.clone()), "the same path again");
        assert!(log.changed(wired), "Wi-Fi to Ethernet");
        assert!(log.changed(wifi.clone()), "and back");
        assert!(log.changed(other_router), "another network on the same interface");
        assert!(log.changed(down), "the path went");
        assert!(log.changed(wifi), "and came back");
    }

    /// A resume fired by hand reaches the sink watching, and none once the watch is dropped.
    #[test]
    fn an_injected_resume_reaches_its_watch_while_it_lives() {
        let source = Injected::default();
        let heard = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&heard);
        let watch = source.watch(Arc::new(move |r: Resume| {
            assert_eq!(r, Resume::Woke);
            counted.fetch_add(1, Ordering::Relaxed);
        }));
        source.fire(Resume::Woke);
        assert_eq!(heard.load(Ordering::Relaxed), 1);
        drop(watch);
        source.fire(Resume::Woke);
        assert_eq!(heard.load(Ordering::Relaxed), 1, "a dropped watch hears nothing");
    }

    /// Every kind reads back from its name, and says whether the device was away or the path
    /// moved.
    #[test]
    fn every_resume_has_a_name_and_says_what_it_means() {
        for r in Resume::ALL {
            assert_eq!(Resume::named(r.name()), Some(r));
        }
        assert!(Resume::Woke.was_away() && Resume::Unlocked.was_away());
        assert!(!Resume::Foreground.was_away() && !Resume::PathChanged.was_away());
        assert!(Resume::PathChanged.moved() && !Resume::Woke.moved());
    }

    /// The system's watch starts and stops on this Mac: its observers are added and removed and
    /// its path monitor starts and is cancelled. Nothing is posted: the Mac neither sleeps nor
    /// changes its network.
    #[test]
    fn the_system_watch_starts_and_stops() {
        let heard = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&heard);
        let watch = System.watch(Arc::new(move |_| {
            counted.fetch_add(1, Ordering::Relaxed);
        }));
        drop(watch);
        assert_eq!(heard.load(Ordering::Relaxed), 0, "the first path is no change");
    }
}

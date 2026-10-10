//! What tells the app the person left this Mac at once, not once it has gone unused a while.
//!
//! The Mac is going to sleep, its screens went to sleep (display sleep, or the lid closed on a
//! Mac kept running), its login session gave way to another, or its screen was locked.
//!
//! Each is a [`Left`] handed to a [`Sink`] from the thread that posted it, the main thread for
//! AppKit's notifications. The way back is a [`Resume`] of the matching kind
//! ([`Left::ended_by`]): woke, screens woke, the session back, unlocked. An iPhone or iPad
//! needs none of it: the system resigns the app when its screen locks.

use std::sync::Arc;

use crate::resume::Resume;

/// A moment the person left this Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Left {
    /// The Mac is going to sleep.
    Sleeps,
    /// The displays went to sleep: display sleep, or the lid closed on a Mac kept running.
    ScreensSlept,
    /// The login session gave way to another (fast user switching).
    SessionResigned,
    /// The screen was locked.
    Locked,
}

impl Left {
    /// Every kind, for a test's sweep.
    pub const ALL: [Self; 4] =
        [Self::Sleeps, Self::ScreensSlept, Self::SessionResigned, Self::Locked];

    /// The resume that ends this leaving: the Mac woke, its screens woke, its session came
    /// back, its screen was unlocked. Screens that wake on a locked Mac end only the sleep.
    #[must_use]
    pub const fn ended_by(self) -> Resume {
        match self {
            Self::Sleeps => Resume::Woke,
            Self::ScreensSlept => Resume::ScreensWoke,
            Self::SessionResigned => Resume::SessionActive,
            Self::Locked => Resume::Unlocked,
        }
    }
}

/// Where leavings go, from whatever thread saw them.
pub type Sink = Arc<dyn Fn(Left) + Send + Sync>;

/// A running watch: leavings reach its sink until it is dropped.
pub struct Watch {
    _held: mac::Held,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch").finish_non_exhaustive()
    }
}

/// Hand every leaving to `sink` from now until the watch is dropped. Watched on the main
/// thread, which AppKit posts them on.
#[must_use]
pub fn watch(sink: &Sink) -> Watch {
    Watch { _held: mac::watch(sink) }
}

mod mac {
    use std::ptr::NonNull;
    use std::sync::Arc;

    use block2::RcBlock;
    use objc2::Message as _;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_app_kit::{
        NSWorkspace, NSWorkspaceScreensDidSleepNotification,
        NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceWillSleepNotification,
    };
    use objc2_foundation::{
        NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSString,
    };

    use super::{Left, Sink};

    /// An observer and the centre it was added to.
    type Observer =
        (Retained<NSNotificationCenter>, Retained<ProtocolObject<dyn NSObjectProtocol>>);

    /// The observers a watch added.
    pub(super) struct Held(Vec<Observer>);

    impl Drop for Held {
        fn drop(&mut self) {
            for (center, observer) in self.0.drain(..) {
                // SAFETY: an observer `addObserverForName:…` returned on this centre, removed
                // once.
                unsafe {
                    center.removeObserver(observer.as_ref());
                }
            }
        }
    }

    pub(super) fn watch(sink: &Sink) -> Held {
        let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
        let distributed = Retained::into_super(NSDistributedNotificationCenter::defaultCenter());
        // SAFETY: immutable `NSString` statics AppKit defines (NSWorkspace.h).
        let (sleeps, screens, session) = unsafe {
            (
                NSWorkspaceWillSleepNotification,
                NSWorkspaceScreensDidSleepNotification,
                NSWorkspaceSessionDidResignActiveNotification,
            )
        };
        // The screen saver's and the login window's lock, posted to every process through the
        // distributed centre; no header names it. Its unlock is a resume's
        // (`crate::resume`).
        let locked = NSString::from_str("com.apple.screenIsLocked");
        let watched = [
            (workspace.clone(), sleeps.retain(), Left::Sleeps),
            (workspace.clone(), screens.retain(), Left::ScreensSlept),
            (workspace, session.retain(), Left::SessionResigned),
            (distributed, locked, Left::Locked),
        ];
        let observers = watched
            .into_iter()
            .map(|(center, name, left)| {
                let sink = Arc::clone(sink);
                let block = RcBlock::new(move |_note: NonNull<NSNotification>| sink(left));
                // SAFETY: with no queue the block runs on the thread that posts; everything it
                // holds is `Send + Sync`, and the observer is removed before the block goes
                // (`Held::drop`).
                let observer = unsafe {
                    center.addObserverForName_object_queue_usingBlock(
                        Some(&name),
                        None,
                        None,
                        &block,
                    )
                };
                (center, observer)
            })
            .collect();
        Held(observers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each leaving ends by its own way back, and no two share one: screens that wake on a
    /// locked Mac do not unlock it.
    #[test]
    fn each_leaving_ends_by_its_own_way_back() {
        let backs: Vec<Resume> = Left::ALL.iter().map(|l| l.ended_by()).collect();
        assert_eq!(
            backs,
            [Resume::Woke, Resume::ScreensWoke, Resume::SessionActive, Resume::Unlocked]
        );
        assert!(backs.iter().all(|r| r.was_away()), "each is a return from away");
    }

    /// A watch adds its observers and takes them away when dropped, with nothing posted.
    #[test]
    fn a_watch_comes_and_goes() {
        let sink: Sink = Arc::new(|_left| {});
        let watch = watch(&sink);
        drop(watch);
    }
}

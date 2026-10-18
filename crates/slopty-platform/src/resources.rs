//! Whether the system would have apps scale back (iOS 27's
//! `systemPrefersReducedResourceUsage`), and word when that or Low Power Mode changes.
//!
//! A remote screen is the heaviest thing Slopty draws: a hardware decode and a present per
//! frame. While the system asks for less, a stream asks for at most 60 frames a second
//! (`slopty_ui::screen`), and Low Power Mode, which lowers a `ProMotion` screen's rate, is
//! followed the same way. macOS has no such ask, so there it is never on.

/// Whether the system prefers reduced resource usage now. Main thread only on iOS; `false` off
/// it and on macOS.
#[must_use]
#[cfg_attr(not(target_os = "ios"), expect(clippy::missing_const_for_fn, reason = "UIKit on iOS"))]
pub fn reduced() -> bool {
    #[cfg(target_os = "ios")]
    {
        use objc2::msg_send;
        use objc2::runtime::Bool;
        let Some(mtm) = objc2::MainThreadMarker::new() else { return false };
        let app = objc2_ui_kit::UIApplication::sharedApplication(mtm);
        // SAFETY: UIApplication.h: `systemPrefersReducedResourceUsage` (iOS 27) is a read-only
        // BOOL property, read on the main thread.
        let reduced: Bool = unsafe { msg_send![&*app, systemPrefersReducedResourceUsage] };
        reduced.as_bool()
    }
    #[cfg(not(target_os = "ios"))]
    false
}

/// Calls `changed` on the main thread each time [`reduced`] or Low Power Mode changes, for as
/// long as it lives.
#[cfg(target_os = "ios")]
pub struct Watch(
    Vec<objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>>,
);

#[cfg(target_os = "ios")]
impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch").finish_non_exhaustive()
    }
}

#[cfg(target_os = "ios")]
#[link(name = "UIKit", kind = "framework")]
unsafe extern "C" {
    /// `UIApplication.h` (iOS 27), which objc2-ui-kit 0.3.2 predates.
    static UIApplicationSystemPrefersReducedResourceUsageDidChangeNotification:
        &'static objc2_foundation::NSString;
}

/// Watch for changes ([`Watch`]).
#[cfg(target_os = "ios")]
pub fn watch(changed: impl Fn() + 'static) -> Watch {
    use std::ptr::NonNull;
    use std::rc::Rc;

    use block2::RcBlock;
    use objc2_foundation::{
        NSNotification, NSNotificationCenter, NSOperationQueue,
        NSProcessInfoPowerStateDidChangeNotification,
    };

    let changed = Rc::new(changed);
    let center = NSNotificationCenter::defaultCenter();
    let main = NSOperationQueue::mainQueue();
    // SAFETY: immutable `NSString` statics UIKit and Foundation define.
    let names = unsafe {
        [
            UIApplicationSystemPrefersReducedResourceUsageDidChangeNotification,
            NSProcessInfoPowerStateDidChangeNotification,
        ]
    };
    Watch(
        names
            .into_iter()
            .map(|name| {
                let changed = Rc::clone(&changed);
                let block = RcBlock::new(move |_note: NonNull<NSNotification>| changed());
                // SAFETY: the main queue runs the block on the main thread, the only one that
                // touches the `Rc` it holds; the observer is removed before the block goes
                // (`Drop`).
                unsafe {
                    center.addObserverForName_object_queue_usingBlock(
                        Some(name),
                        None,
                        Some(&main),
                        &block,
                    )
                }
            })
            .collect(),
    )
}

#[cfg(target_os = "ios")]
impl Drop for Watch {
    fn drop(&mut self) {
        let center = objc2_foundation::NSNotificationCenter::defaultCenter();
        for observer in self.0.drain(..) {
            // SAFETY: an observer `addObserverForName:…` returned, removed once.
            unsafe {
                center.removeObserver(observer.as_ref());
            }
        }
    }
}

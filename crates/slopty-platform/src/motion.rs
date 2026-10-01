//! The system's Reduce Motion and Increase Contrast settings, heard as they change.
//!
//! [`crate::reduce_motion`] keeps its answer for a second; a change in System Settings (or iOS
//! Settings) is heard at once here, so a spring already under way stops gliding and GPUI's own
//! animations follow in the next frame. Increase Contrast (iOS: Darker System Colors) is heard
//! the same way, so the theme's contrasted variant replaces the plain one in the next frame.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSOperationQueue};

/// A watch on the setting: it stops when dropped.
#[must_use = "the watch stops when dropped"]
#[derive(Debug)]
pub struct Watch {
    center: Retained<NSNotificationCenter>,
    observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

/// Call `changed` with the setting each time the system says it changed, on the main thread,
/// and keep it for [`crate::reduce_motion`].
pub fn watch_reduce_motion(changed: impl Fn(bool) + 'static) -> Watch {
    #[cfg(target_os = "macos")]
    let (center, name) = (
        objc2_app_kit::NSWorkspace::sharedWorkspace().notificationCenter(),
        // SAFETY: an immutable `NSString` static AppKit defines (NSAccessibility.h).
        unsafe { objc2_app_kit::NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification },
    );
    #[cfg(target_os = "ios")]
    let (center, name) = (
        NSNotificationCenter::defaultCenter(),
        // SAFETY: an immutable `NSString` static UIKit defines (UIAccessibility.h).
        unsafe { objc2_ui_kit::UIAccessibilityReduceMotionStatusDidChangeNotification },
    );
    observe(center, name, move || {
        let on = crate::system_reduce_motion();
        crate::REDUCE_MOTION.set(crate::since_epoch(), on);
        changed(on);
    })
}

/// Call `changed` with whether the system asks for more contrast each time that changes, on
/// the main thread.
pub fn watch_increase_contrast(changed: impl Fn(bool) + 'static) -> Watch {
    #[cfg(target_os = "macos")]
    let (center, name) = (
        objc2_app_kit::NSWorkspace::sharedWorkspace().notificationCenter(),
        // SAFETY: an immutable `NSString` static AppKit defines (NSAccessibility.h).
        unsafe { objc2_app_kit::NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification },
    );
    #[cfg(target_os = "ios")]
    let (center, name) = (
        NSNotificationCenter::defaultCenter(),
        // SAFETY: an immutable `NSString` static UIKit defines (UIAccessibility.h).
        unsafe { objc2_ui_kit::UIAccessibilityDarkerSystemColorsStatusDidChangeNotification },
    );
    let last = std::cell::Cell::new(increase_contrast());
    observe(center, name, move || {
        // AppKit posts one notification for every display option: only a change of this one
        // is passed on.
        let on = increase_contrast();
        if on != last.replace(on) {
            changed(on);
        }
    })
}

/// Whether the system asks for more contrast: macOS's Increase Contrast, iOS's Darker System
/// Colors. One AppKit or UIKit read; ask once, then follow [`watch_increase_contrast`].
#[must_use]
pub fn increase_contrast() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldIncreaseContrast()
    }
    #[cfg(target_os = "ios")]
    {
        objc2_ui_kit::UIAccessibilityDarkerSystemColorsEnabled()
    }
}

/// Run `heard` on the main thread each time `center` posts `name`, until the watch drops.
fn observe(
    center: Retained<NSNotificationCenter>,
    name: &objc2_foundation::NSNotificationName,
    heard: impl Fn() + 'static,
) -> Watch {
    let block = RcBlock::new(move |_note: NonNull<NSNotification>| heard());
    let main = NSOperationQueue::mainQueue();
    // SAFETY: the block runs on the main queue, the thread `heard` was made for, whatever
    // thread posts; the observer is removed before the block goes (`Drop`).
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(Some(name), None, Some(&main), &block)
    };
    Watch { center, observer }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY: an observer `addObserverForName:…` returned, removed once.
        unsafe {
            self.center.removeObserver(self.observer.as_ref());
        }
    }
}

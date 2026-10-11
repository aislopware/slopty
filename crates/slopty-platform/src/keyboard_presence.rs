//! A hardware keyboard coming and going on iOS, heard as `GameController` says it rather than
//! asked about on a timer. A Mac always has one; it builds this so its tests can post the
//! notifications.
//!
//! The system coalesces every keyboard into one (`GCKeyboard.coalescedKeyboard`), so it posts
//! `GCKeyboardDidConnectNotification` when the first connects and
//! `GCKeyboardDidDisconnectNotification` when the last goes (`GCKeyboard.h`). A change while the
//! app is away may go unheard, so the app reads [`crate::hardware_keyboard_attached`] again on
//! each return to the front.

use std::rc::Rc;

use objc2_foundation::NSNotificationCenter;
use objc2_game_controller::{
    GCKeyboardDidConnectNotification, GCKeyboardDidDisconnectNotification,
};

use crate::motion;

/// A watch on the keyboard: it stops when dropped.
#[must_use = "the watch stops when dropped"]
#[derive(Debug)]
pub struct Watch {
    _connect: motion::Watch,
    _disconnect: motion::Watch,
}

/// Call `changed` on the main thread with whether a hardware keyboard is attached, each time
/// the first one connects (true) or the last one goes (false).
pub fn watch(changed: impl Fn(bool) + 'static) -> Watch {
    let changed = Rc::new(changed);
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: an immutable `NSString` static GameController defines (GCKeyboard.h).
    let connected = unsafe { GCKeyboardDidConnectNotification };
    // SAFETY: as above.
    let gone = unsafe { GCKeyboardDidDisconnectNotification };
    let on = Rc::clone(&changed);
    Watch {
        _connect: motion::observe(center.clone(), connected, move || on(true)),
        _disconnect: motion::observe(center, gone, move || changed(false)),
    }
}

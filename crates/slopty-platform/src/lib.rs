//! Process-level platform helpers: keep the process out of App Nap and timer coalescing while
//! a session is live, and raise the user's attention.
//!
//! macOS throttles timers and network work of applications whose windows are occluded or
//! hidden (App Nap) and coalesces timers of background processes. iroh's per-path heartbeat is
//! 5 s against a 15 s idle timeout, so a throttled peer can stop answering on its direct path
//! long enough for the other side to abandon it and fall back to the relay. An [`Activity`]
//! with `LatencyCritical` tells the system this process must keep its timers sharp; the app
//! and the host daemon hold one for their whole run. (A 43 s direct → relay flap was seen on
//! 2026-09-04; covering the window for 70 s did *not* reproduce it, so this is a precaution,
//! not the confirmed cause. Path events are logged on both ends to catch the next one.)

#![cfg(any(target_os = "macos", target_os = "ios"))]

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

/// A live `NSProcessInfo` activity; dropping it ends the activity.
#[derive(Debug)]
pub struct Activity {
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Activity {
    /// Declare user-interactive, latency-critical work with a human-readable reason (shown by
    /// Activity Monitor and `pmset -g assertions`). Allows idle system sleep.
    #[must_use]
    pub fn latency_critical(reason: &str) -> Self {
        let options = NSActivityOptions::UserInitiatedAllowingIdleSystemSleep
            | NSActivityOptions::LatencyCritical;
        let token = NSProcessInfo::processInfo()
            .beginActivityWithOptions_reason(options, &NSString::from_str(reason));
        Self { token }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        // SAFETY: `token` is exactly the object `beginActivityWithOptions:reason:` returned,
        // which is what `endActivity:` requires.
        unsafe { NSProcessInfo::processInfo().endActivity(&self.token) }
    }
}

/// Get the human's attention: the user's alert sound on macOS, a vibration on iOS.
///
/// Fire-and-forget; the system sound server plays asynchronously.
pub fn attention() {
    #[cfg(target_os = "macos")]
    // SAFETY: AudioToolbox documents `AudioServicesPlayAlertSound` as callable from any thread
    // with any `SystemSoundID`; `kSystemSoundID_UserPreferredAlert` is the constant it names for
    // the alert chosen in System Settings.
    unsafe {
        objc2_audio_toolbox::AudioServicesPlayAlertSound(
            objc2_audio_toolbox::kSystemSoundID_UserPreferredAlert,
        );
    }
    #[cfg(target_os = "ios")]
    // SAFETY: as above; `kSystemSoundID_Vibrate` is the documented constant for the vibration
    // pattern (a no-op on devices without a vibrator, such as the simulator).
    unsafe {
        objc2_audio_toolbox::AudioServicesPlaySystemSound(
            objc2_audio_toolbox::kSystemSoundID_Vibrate,
        );
    }
}

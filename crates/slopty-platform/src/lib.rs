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

/// Put the process in the `Playback` audio session category, mixing with other apps.
///
/// Remote-window audio then plays through the ring switch and alongside music. macOS has no
/// audio session; the call is a no-op there.
#[cfg_attr(target_os = "macos", expect(clippy::missing_const_for_fn, reason = "a no-op here"))]
pub fn playback_audio_session() {
    #[cfg(target_os = "ios")]
    {
        use objc2_avf_audio::{
            AVAudioSession, AVAudioSessionCategoryOptions, AVAudioSessionCategoryPlayback,
        };
        // SAFETY: AVFoundation rule: the shared session is created on first use from any
        // thread; the category constant is the framework's own static (null only if the
        // framework failed to load, which `Option` covers).
        let session = unsafe { AVAudioSession::sharedInstance() };
        // SAFETY: as above.
        let Some(category) = (unsafe { AVAudioSessionCategoryPlayback }) else { return };
        // SAFETY: category and options are valid for `setCategory:withOptions:error:`.
        let set = unsafe {
            session.setCategory_withOptions_error(
                category,
                AVAudioSessionCategoryOptions::MixWithOthers,
            )
        };
        if let Err(e) = set {
            tracing::warn!(error = %e, "audio session category");
            return;
        }
        // SAFETY: activating the configured shared session.
        if let Err(e) = unsafe { session.setActive_error(true) } {
            tracing::warn!(error = %e, "audio session activate");
        }
    }
}

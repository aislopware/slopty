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

/// Show how many sessions are waiting on the human on the app icon.
///
/// The Dock badge on macOS (cleared at zero). iOS keeps the count inside the app; its icon
/// badge needs notification authorisation, which nothing else here asks for.
///
/// Main thread only (AppKit); called from GPUI's main-thread callbacks. Off it, this is a no-op.
pub fn set_badge(count: usize) {
    #[cfg(target_os = "macos")]
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
        let tile = objc2_app_kit::NSApplication::sharedApplication(mtm).dockTile();
        tile.setBadgeLabel(label.as_deref());
        tracing::debug!(count, badge = ?tile.badgeLabel(), "dock badge");
    } else {
        tracing::warn!(count, "dock badge skipped: not on the main thread");
    }
    #[cfg(target_os = "ios")]
    tracing::trace!(count, "icon badge needs notification authorisation; kept in-app");
}

/// Bounce the Dock icon until the app is activated.
///
/// macOS only, and a no-op when the app is already active. Pairs with [`attention`] for an
/// agent that needs the human.
#[cfg_attr(target_os = "ios", expect(clippy::missing_const_for_fn, reason = "a no-op here"))]
pub fn bounce() {
    #[cfg(target_os = "macos")]
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        if !app.isActive() {
            // The returned request id is only for cancelling early; the activation cancels it.
            let request = app
                .requestUserAttention(objc2_app_kit::NSRequestUserAttentionType::CriticalRequest);
            tracing::trace!(request, "dock bounce");
        }
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

/// Whether a physical keyboard is attached. On iOS this is `GameController`'s coalesced keyboard
/// (nil until one connects over Smart Connector, Bluetooth or USB); a Mac always has one.
#[cfg_attr(not(target_os = "ios"), expect(clippy::missing_const_for_fn, reason = "constant here"))]
pub fn hardware_keyboard_attached() -> bool {
    #[cfg(target_os = "ios")]
    {
        // SAFETY: GameController rule: `coalescedKeyboard` is a class property readable from
        // any thread; it is nil when no keyboard is connected, which `Option` covers.
        unsafe { objc2_game_controller::GCKeyboard::coalescedKeyboard() }.is_some()
    }
    #[cfg(not(target_os = "ios"))]
    {
        true
    }
}

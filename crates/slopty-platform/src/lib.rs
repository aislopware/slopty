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
        Self::begin(options, reason)
    }

    /// Keep the machine from idle sleep while the reason holds (the display may still sleep):
    /// a client is attached and the host has to keep answering it.
    #[must_use]
    pub fn system_awake(reason: &str) -> Self {
        Self::begin(NSActivityOptions::UserInitiated, reason)
    }

    /// Keep the display awake as well: a window or display is being captured, and a sleeping
    /// display captures nothing.
    #[must_use]
    pub fn display_awake(reason: &str) -> Self {
        let options =
            NSActivityOptions::UserInitiated | NSActivityOptions::IdleDisplaySleepDisabled;
        Self::begin(options, reason)
    }

    fn begin(options: NSActivityOptions, reason: &str) -> Self {
        let token = NSProcessInfo::processInfo()
            .beginActivityWithOptions_reason(options, &NSString::from_str(reason));
        Self { token }
    }
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the token is only ever handed back to NSProcessInfo, which is thread-safe"
)]
// SAFETY: `NSProcessInfo` is thread-safe (Foundation's NSProcessInfo documentation: thread-safe
// since macOS 10.7) and `endActivity:` takes its token back from any thread; the token is an
// opaque object Foundation owns and this type only ever hands it back.
unsafe impl Send for Activity {}
// SAFETY: as above; the only shared access is `Drop`, which needs `&mut self`.
unsafe impl Sync for Activity {}

impl Drop for Activity {
    fn drop(&mut self) {
        // SAFETY: `token` is exactly the object `beginActivityWithOptions:reason:` returned,
        // which is what `endActivity:` requires.
        unsafe { NSProcessInfo::processInfo().endActivity(&self.token) }
    }
}

/// Get the human's attention: the user's alert sound on macOS, a haptic on iOS.
///
/// Fire-and-forget; the system sound server plays asynchronously. On iOS the Taptic
/// engine's "warning" notification pattern (`UINotificationFeedbackGenerator`, main thread
/// only, a no-op on the simulator and on a phone whose haptics are off); off the main thread
/// the old vibration pattern, which any thread may ask for.
pub fn attention() {
    #[cfg(target_os = "ios")]
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let generator = objc2_ui_kit::UINotificationFeedbackGenerator::new(mtm);
        generator.notificationOccurred(objc2_ui_kit::UINotificationFeedbackType::Warning);
        tracing::debug!("attention: notification haptic");
        return;
    }
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
    // pattern (a no-op on devices without a vibrator, such as the simulator). Reached only off
    // the main thread, where the feedback generator may not be made.
    unsafe {
        objc2_audio_toolbox::AudioServicesPlaySystemSound(
            objc2_audio_toolbox::kSystemSoundID_Vibrate,
        );
    }
}

/// Hide the pointer until it next moves.
///
/// What Terminal.app does on a keystroke, so the arrow does not sit over the text being
/// typed. A no-op on iOS (no pointer to hide; an iPad's pointer is the system's), and
/// without a running `NSApplication` (a headless test: the call then stalls for seconds
/// waiting on a window server connection).
#[cfg_attr(target_os = "ios", expect(clippy::missing_const_for_fn, reason = "a no-op here"))]
pub fn hide_pointer_until_moved() {
    #[cfg(target_os = "macos")]
    if let Some(mtm) = objc2::MainThreadMarker::new()
        && objc2_app_kit::NSApplication::sharedApplication(mtm).isRunning()
    {
        objc2_app_kit::NSCursor::setHiddenUntilMouseMoves(true);
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

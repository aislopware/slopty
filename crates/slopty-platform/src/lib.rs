//! Process-level platform helpers: keep the process out of App Nap and timer coalescing while
//! a session is live, and raise the user's attention.
//!
//! macOS throttles timers and network work of applications whose windows are occluded or
//! hidden (App Nap) and coalesces timers of background processes. The QUIC keep-alive is 5 s
//! and the app gives a silent worker up after 15 s, so a throttled peer can go quiet long enough
//! for the other side to drop the link. An [`Activity`] with `LatencyCritical` tells the system
//! this process must keep its timers sharp; the app and the worker daemon hold one for their
//! whole run.

#![cfg(any(target_os = "macos", target_os = "ios"))]

#[cfg(target_os = "macos")]
pub mod drag;
pub mod file_drop;
pub mod pasteboard;
pub mod web;

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
    /// a client is attached and the worker has to keep answering it.
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

/// Whether the ⌥ key down in the event being handled is the right one.
///
/// AppKit's `modifierFlags` carry the device-dependent side bits GPUI drops; read off the
/// application's current event, so only meaningful from a key handler on the main thread.
/// `false` on iOS (no sides) and off the main thread.
#[cfg_attr(target_os = "ios", expect(clippy::missing_const_for_fn, reason = "a no-op here"))]
#[must_use]
pub fn right_option_held() -> bool {
    #[cfg(target_os = "macos")]
    {
        // `NX_DEVICERALTKEYMASK` from `<IOKit/hidsystem/IOLLEvent.h>`: the right Option key's
        // device-dependent bit in `NSEvent.modifierFlags` (not in AppKit's public flags).
        const NX_DEVICERALTKEYMASK: usize = 0x0000_0040;
        objc2::MainThreadMarker::new()
            .and_then(|mtm| objc2_app_kit::NSApplication::sharedApplication(mtm).currentEvent())
            .is_some_and(|event| event.modifierFlags().0 & NX_DEVICERALTKEYMASK != 0)
    }
    #[cfg(target_os = "ios")]
    false
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

/// Open `url` in the default browser (a forwarded port: the browser's own devtools, extensions
/// and passwords come with it).
///
/// Main thread only on iOS (`UIApplication`); a no-op off it there.
pub fn open_url(url: &str) {
    let Some(url) = objc2_foundation::NSURL::URLWithString(&NSString::from_str(url)) else {
        tracing::warn!(url, "not a URL");
        return;
    };
    #[cfg(target_os = "macos")]
    {
        let opened = objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
        tracing::debug!(opened, "open url");
    }
    #[cfg(target_os = "ios")]
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let app = objc2_ui_kit::UIApplication::sharedApplication(mtm);
        let options = objc2_foundation::NSDictionary::new();
        // SAFETY: UIKit rule: `openURL:options:completionHandler:` on the main thread with a
        // valid URL, an empty options dictionary and no completion handler.
        unsafe {
            app.openURL_options_completionHandler(&url, &options, None);
        }
    }
}

/// How long [`reduce_motion`] trusts its last read of the system setting.
pub const REDUCE_MOTION_FRESH: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether the system asks for motion to be reduced, as read at most [`REDUCE_MOTION_FRESH`]
/// ago.
///
/// The canvas and the terminal ask on every frame, so the answer is kept rather than asked of
/// AppKit each time; a change in System Settings shows within a second. This crate does not
/// watch the system's change notification, so a clock it is. The canvas is what the setting
/// governs: pan momentum and zoom settling are the only motion Slopty invents, and a person
/// who turned this on wants the view where they put it rather than gliding there.
pub fn reduce_motion() -> bool {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    static KEPT: Kept = Kept::new();
    let now = EPOCH.get_or_init(std::time::Instant::now).elapsed();
    KEPT.get(now, REDUCE_MOTION_FRESH, system_reduce_motion)
}

/// A yes or no read from the system and trusted for a while, shared without a lock.
struct Kept {
    /// Nanoseconds after the caller's epoch of the last read; `u64::MAX` before the first.
    read_at: std::sync::atomic::AtomicU64,
    value: std::sync::atomic::AtomicBool,
}

impl Kept {
    const fn new() -> Self {
        Self {
            read_at: std::sync::atomic::AtomicU64::new(u64::MAX),
            value: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The kept answer at `now` (time since the caller's epoch), or `read`'s when the kept one
    /// is `fresh` old or older. Two threads racing past a stale answer both read and store the
    /// same thing.
    fn get(
        &self,
        now: std::time::Duration,
        fresh: std::time::Duration,
        read: fn() -> bool,
    ) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        let nanos = |d: std::time::Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        let now = nanos(now).min(u64::MAX.saturating_sub(1));
        let read_at = self.read_at.load(Relaxed);
        if read_at != u64::MAX && now.saturating_sub(read_at) < nanos(fresh) {
            return self.value.load(Relaxed);
        }
        let value = read();
        self.value.store(value, Relaxed);
        self.read_at.store(now, Relaxed);
        value
    }
}

/// The setting as the system holds it now: one AppKit or UIKit property read.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "ios")),
    expect(clippy::missing_const_for_fn, reason = "constant only off Apple platforms")
)]
fn system_reduce_motion() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }
    #[cfg(target_os = "ios")]
    {
        objc2_ui_kit::UIAccessibilityIsReduceMotionEnabled()
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        false
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use super::Kept;

    /// The answer is read once and kept until it is a whole `fresh` old, then read again.
    #[test]
    fn an_answer_is_kept_until_it_goes_stale() {
        static READS: AtomicU32 = AtomicU32::new(0);
        fn read() -> bool {
            READS.fetch_add(1, Ordering::Relaxed).is_multiple_of(2)
        }
        let kept = Kept::new();
        let fresh = Duration::from_secs(1);
        let at = Duration::from_millis;
        assert!(kept.get(at(5_000), fresh, read), "the first call reads");
        assert!(kept.get(at(5_999), fresh, read), "kept");
        assert_eq!(READS.load(Ordering::Relaxed), 1);
        assert!(!kept.get(at(6_000), fresh, read), "a second on, read again");
        assert_eq!(READS.load(Ordering::Relaxed), 2);
    }

    /// What one read of the system setting costs against one read of the kept answer; prints
    /// the numbers MEASUREMENTS records (`cargo test -p slopty-platform --release --lib
    /// reduce_motion -- --nocapture`). The kept answer is the system's.
    #[test]
    fn reduce_motion_is_kept_as_the_system_says() {
        const READS: u32 = 100_000;
        let time = |read: fn() -> bool| {
            let started = Instant::now();
            let mut on = 0_u32;
            for _ in 0..READS {
                on = on.wrapping_add(u32::from(std::hint::black_box(read())));
            }
            std::hint::black_box(on);
            started.elapsed() / READS
        };
        let asked = time(super::system_reduce_motion);
        let kept = time(super::reduce_motion);
        println!(
            "MEASURE reduce motion: asking the system {asked:?} a read, the kept answer {kept:?}"
        );
        assert_eq!(super::reduce_motion(), super::system_reduce_motion());
    }
}

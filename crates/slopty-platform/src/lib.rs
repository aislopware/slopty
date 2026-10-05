//! Process-level platform helpers: keep the process out of App Nap and timer coalescing while
//! a session is live, and raise the user's attention.
//!
//! macOS throttles timers and network work of applications whose windows are occluded or
//! hidden (App Nap) and coalesces timers of background processes. The app gives a silent worker
//! up after a handful of missed QUIC keep-alives (`slopty_net::endpoint::KEEP_ALIVE`, and the
//! app's `SILENCE_DROP`), so a peer whose timers are throttled by even a few seconds can go
//! quiet long enough for the other side to drop the link. An [`Activity`] with
//! `LatencyCritical` tells the system this process must keep its timers sharp; the app and the
//! worker daemon hold one for their whole run.
//!
//! The worker's and the server's half of this crate also builds for Linux: [`Activity`],
//! [`user_interactive_thread`], [`open_url`], [`dirs`], [`fs`], [`service`] and `trash`. What Linux
//! cannot do yet says so where it is asked (`docs/decisions/platform.md`, "Linux seams"). The
//! client's half (the pasteboard, drops, the browser tile, the Dock) is Apple-only.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod continued;
pub mod dirs;
#[cfg(target_vendor = "apple")]
pub mod dock;
#[cfg(target_os = "macos")]
pub mod drag;
#[cfg(target_vendor = "apple")]
pub mod fetch;
#[cfg(target_vendor = "apple")]
pub mod file_drop;
#[cfg(target_os = "macos")]
pub mod files;
pub mod fs;
#[cfg(target_os = "macos")]
pub mod idle;
#[cfg(target_os = "macos")]
pub mod input_source;
#[cfg(target_vendor = "apple")]
pub mod keyboard;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_vendor = "apple")]
pub mod motion;
#[cfg(target_vendor = "apple")]
pub mod notify;
#[cfg(target_os = "ios")]
pub mod paste_control;
#[cfg(target_vendor = "apple")]
pub mod pasteboard;
pub mod pasteboard_access;
#[cfg(target_os = "macos")]
pub mod power;
pub mod privacy;
#[cfg(target_os = "macos")]
pub mod proc_files;
pub mod resume;
#[cfg(target_vendor = "apple")]
pub mod secure_input;
pub mod service;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod symbols;
#[cfg(target_vendor = "apple")]
pub mod system_keys;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod trash;
#[cfg(target_vendor = "apple")]
pub mod web;

#[cfg(target_os = "linux")]
pub use linux::{Activity, open_url, open_url_behind, user_interactive_thread};
#[cfg(target_vendor = "apple")]
use objc2::rc::Retained;
#[cfg(target_vendor = "apple")]
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
#[cfg(target_vendor = "apple")]
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

/// A live `NSProcessInfo` activity; dropping it ends the activity.
#[cfg(target_vendor = "apple")]
#[derive(Debug)]
pub struct Activity {
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

#[cfg(target_vendor = "apple")]
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

#[cfg(target_vendor = "apple")]
#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the token is only ever handed back to NSProcessInfo, which is thread-safe"
)]
// SAFETY: `NSProcessInfo` is thread-safe (Foundation's NSProcessInfo documentation: thread-safe
// since macOS 10.7) and `endActivity:` takes its token back from any thread; the token is an
// opaque object Foundation owns and this type only ever hands it back.
unsafe impl Send for Activity {}
#[cfg(target_vendor = "apple")]
// SAFETY: as above; the only shared access is `Drop`, which needs `&mut self`.
unsafe impl Sync for Activity {}

#[cfg(target_vendor = "apple")]
impl Drop for Activity {
    fn drop(&mut self) {
        // SAFETY: `token` is exactly the object `beginActivityWithOptions:reason:` returned,
        // which is what `endActivity:` requires.
        unsafe { NSProcessInfo::processInfo().endActivity(&self.token) }
    }
}

/// Put the calling thread in `QOS_CLASS_USER_INTERACTIVE`, the class of the work a person is
/// waiting on, for the threads a keystroke, its echo or a remote window's input crosses.
///
/// A thread nobody classed runs below a build's user-initiated threads, and an [`Activity`]
/// classes no thread: under an all-core spin at `USER_INITIATED`, the worker's share of an echo
/// went from p90 5–6 ms unclassed to 0.2–0.5 ms classed, and the CLI's whole echo from p90
/// 166–270 ms to 20–23 ms (MEASUREMENTS.md, "the keystroke path under an all-core spin").
/// Never called on a thread that is already realtime (audio) or on the main thread of an app,
/// which the system runs user-interactive already.
#[cfg(target_vendor = "apple")]
pub fn user_interactive_thread() {
    // SAFETY: `pthread_set_qos_class_self_np` (pthread/qos.h) changes only the calling thread's
    // own class, and a relative priority of 0 is within every class's range
    // (`QOS_MIN_RELATIVE_PRIORITY` is -15).
    let refused = unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
    };
    if refused != 0 {
        tracing::warn!(error = refused, "user-interactive QoS refused");
    }
}

/// Get the human's attention: the user's alert sound on macOS, a haptic on iOS.
///
/// Fire-and-forget; the system sound server plays asynchronously. On iOS the Taptic
/// engine's "warning" notification pattern (`UINotificationFeedbackGenerator`, main thread
/// only, a no-op on the simulator and on a phone whose haptics are off); off the main thread
/// the old vibration pattern, which any thread may ask for.
#[cfg(target_vendor = "apple")]
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
#[cfg(target_vendor = "apple")]
#[cfg_attr(target_os = "ios", expect(clippy::missing_const_for_fn, reason = "a no-op here"))]
pub fn hide_pointer_until_moved() {
    #[cfg(target_os = "macos")]
    if let Some(mtm) = objc2::MainThreadMarker::new()
        && objc2_app_kit::NSApplication::sharedApplication(mtm).isRunning()
    {
        objc2_app_kit::NSCursor::setHiddenUntilMouseMoves(true);
    }
}

/// The refresh period of the screen the app draws on.
///
/// The main screen on macOS (the one with the key window), the first window scene's screen on
/// iOS. Read from its most frames a second, so a `ProMotion` panel reads 8.3 ms whatever rate
/// it idles at. Main thread only (AppKit, UIKit); `None` off it, and before a screen or scene
/// exists.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn display_refresh() -> Option<std::time::Duration> {
    let mtm = objc2::MainThreadMarker::new()?;
    #[cfg(target_os = "macos")]
    let fps = objc2_app_kit::NSScreen::mainScreen(mtm)?.maximumFramesPerSecond();
    #[cfg(target_os = "ios")]
    let fps = {
        let scenes = objc2_ui_kit::UIApplication::sharedApplication(mtm).connectedScenes();
        let scene = scenes.iter().find_map(|s| s.downcast::<objc2_ui_kit::UIWindowScene>().ok())?;
        scene.screen().maximumFramesPerSecond()
    };
    let fps = u32::try_from(fps).ok()?;
    std::time::Duration::from_secs(1).checked_div(fps)
}

/// The refresh period of the screen with the CoreGraphics display id `display`.
///
/// That is the screen a window is on, as GPUI names it (`Window::display`). It is read from the
/// screen's most frames a second, as [`display_refresh`] reads the main screen. iOS has one
/// screen per scene and reads [`display_refresh`]. Main thread only; `None` off it and for a
/// screen no longer attached.
#[cfg(target_vendor = "apple")]
#[cfg_attr(target_os = "ios", expect(unused_variables, reason = "one screen a scene"))]
#[must_use]
pub fn display_refresh_of(display: u32) -> Option<std::time::Duration> {
    #[cfg(target_os = "macos")]
    {
        let mtm = objc2::MainThreadMarker::new()?;
        // `<AppKit/NSScreen.h>`, `deviceDescription`: the screen's `CGDirectDisplayID` sits under
        // this key, which the SDK exports no constant for.
        let key = objc2_foundation::ns_string!("NSScreenNumber");
        let screen = objc2_app_kit::NSScreen::screens(mtm).iter().find(|screen| {
            screen
                .deviceDescription()
                .objectForKey(key)
                .and_then(|number| number.downcast::<objc2_foundation::NSNumber>().ok())
                .is_some_and(|number| number.unsignedIntValue() == display)
        })?;
        let fps = u32::try_from(screen.maximumFramesPerSecond()).ok()?;
        std::time::Duration::from_secs(1).checked_div(fps)
    }
    #[cfg(target_os = "ios")]
    {
        display_refresh()
    }
}

/// Whether the ⌥ key down in the event being handled is the right one.
///
/// AppKit's `modifierFlags` carry the device-dependent side bits GPUI drops; read off the
/// application's current event, so only meaningful from a key handler on the main thread.
/// `false` on iOS (no sides) and off the main thread.
#[cfg(target_vendor = "apple")]
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
/// The Dock badge on macOS (cleared at zero). On iOS the icon badge needs notification
/// authorisation, which `notify::System` asks for with its first note; until then the count
/// shows only inside the app.
///
/// Main thread only (AppKit); called from GPUI's main-thread callbacks. Off it, this is a no-op.
#[cfg(target_vendor = "apple")]
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
#[cfg(target_vendor = "apple")]
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

/// The device I/O buffer playback asks iOS for: 10 ms, half a 20 ms Opus packet.
#[cfg(target_os = "ios")]
const IO_BUFFER_SECONDS: f64 = 0.01;

/// Put the process in the `Playback` audio session category, mixing with other apps.
///
/// Remote-window audio then plays through the ring switch and alongside music. macOS has no
/// audio session; the call is a no-op there.
#[cfg(target_vendor = "apple")]
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
        // The player renders straight from its jitter ring on the device's I/O thread, so the
        // I/O buffer is latency the listener hears; iOS defaults to about 23 ms (1024 frames).
        // SAFETY: AVFoundation rule: a preferred duration may be set on the shared session
        // before it is activated; the hardware may pick a different one, which is only a hint lost.
        if let Err(e) = unsafe { session.setPreferredIOBufferDuration_error(IO_BUFFER_SECONDS) } {
            tracing::warn!(error = %e, "audio session I/O buffer");
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
#[cfg(target_vendor = "apple")]
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

/// Open `url` in the default browser without bringing it forward.
///
/// The person keeps watching what they were (a remote display), and the page waits behind. As
/// [`open_url`] on iOS, which opens in front or not at all.
#[cfg(target_vendor = "apple")]
pub fn open_url_behind(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let Some(ns_url) = objc2_foundation::NSURL::URLWithString(&NSString::from_str(url)) else {
            tracing::warn!(url, "not a URL");
            return;
        };
        let config = objc2_app_kit::NSWorkspaceOpenConfiguration::configuration();
        config.setActivates(false);
        objc2_app_kit::NSWorkspace::sharedWorkspace()
            .openURL_configuration_completionHandler(&ns_url, &config, None);
        tracing::debug!("open url behind");
    }
    #[cfg(target_os = "ios")]
    open_url(url);
}

/// The name a person gave this machine, as a client lists it: the computer name on macOS
/// (`scutil --get ComputerName`), the host name on Linux. `None` when it cannot be read.
#[must_use]
pub fn computer_name() -> Option<String> {
    #[cfg(target_os = "linux")]
    let name = rustix::system::uname().nodename().to_string_lossy().into_owned();
    #[cfg(not(target_os = "linux"))]
    let name = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    Some(name.trim().to_owned()).filter(|s| !s.is_empty())
}

/// How long [`reduce_motion`] trusts its last read of the system setting.
pub const REDUCE_MOTION_FRESH: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether the system asks for motion to be reduced, as read at most [`REDUCE_MOTION_FRESH`]
/// ago, or as its change notification last said ([`motion::watch_reduce_motion`]).
///
/// The workspace and the terminal ask on every frame, so the answer is kept rather than asked
/// of AppKit each time. The workspace is what the setting governs: its springs (a column
/// settling, the view offset gliding along the strip, the overview opening and closing) are the
/// motion Slopty invents, and a person who turned this on wants the view where it is going
/// rather than gliding there.
pub fn reduce_motion() -> bool {
    REDUCE_MOTION.get(since_epoch(), REDUCE_MOTION_FRESH, system_reduce_motion)
}

/// The kept answer of [`reduce_motion`].
static REDUCE_MOTION: Kept = Kept::new();

/// Time since the first ask, the clock [`REDUCE_MOTION`] is kept by.
fn since_epoch() -> std::time::Duration {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(std::time::Instant::now).elapsed()
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

    /// Keep `value`, read at `now`.
    #[cfg_attr(
        not(target_vendor = "apple"),
        expect(dead_code, reason = "no watch off Apple platforms")
    )]
    fn set(&self, now: std::time::Duration, value: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        let now = u64::try_from(now.as_nanos()).unwrap_or(u64::MAX).min(u64::MAX.saturating_sub(1));
        self.value.store(value, Relaxed);
        self.read_at.store(now, Relaxed);
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

    /// A thread that asks is user-interactive afterwards, and one that did not keeps the class
    /// it was spawned with.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_thread_asks_for_user_interactive() {
        fn class() -> libc::qos_class_t {
            let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
            let mut relative = 0;
            // SAFETY: `pthread_self` (pthread.h) has no preconditions.
            let me = unsafe { libc::pthread_self() };
            // SAFETY: `pthread_get_qos_class_np` (pthread/qos.h) writes its two out-parameters
            // for a live thread; `me` is the calling thread and both pointers are to locals.
            let failed =
                unsafe { libc::pthread_get_qos_class_np(me, &raw mut class, &raw mut relative) };
            assert_eq!(failed, 0);
            class
        }
        let (before, after) = std::thread::spawn(|| {
            let before = class();
            super::user_interactive_thread();
            (before, class())
        })
        .join()
        .unwrap();
        assert!(!matches!(before, libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE), "{before:?}");
        assert!(matches!(after, libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE), "{after:?}");
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

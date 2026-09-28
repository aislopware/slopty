//! Local notifications: a banner and a sound when something wants the human while the app is
//! not in front, the tap that brings the app back where it points, and the icon badge.
//!
//! Everything goes through [`Notifier`]. What to say, and when, is decided elsewhere (the
//! workspace's `attention`) and tested against [`Memory`], so no test ever raises the system's
//! authorisation prompt or a banner. [`System`] is `UNUserNotificationCenter`, the same on macOS
//! and iOS. It asks for authorisation once, lazily: on the first note it posts, never before.
//! Making one only installs the delegate taps arrive through and reads the settings, and neither
//! prompts.
//!
//! The delegate is the process's, installed once by [`install`] while the app finishes launching:
//! the system hands a delegate installed any later no tap that launched the app. Taps wait in a
//! process-wide queue until the app listens ([`taps`]).
//!
//! `BackgroundGrace` keeps an iOS app running for the short time the system grants after it
//! leaves the screen, so the links stay up and what arrives just after the phone is pocketed
//! still notifies.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

/// One notification.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Note {
    /// Its identifier: a later note with the same one replaces it, and [`Notifier::withdraw`]
    /// takes it back.
    pub id: String,
    /// The first line.
    pub title: String,
    /// What follows it.
    pub body: String,
    /// What a tap hands back ([`Tap::info`]), carried in the notification's `userInfo`.
    pub info: BTreeMap<String, String>,
}

/// A notification the human tapped.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Tap {
    /// The note's identifier.
    pub id: String,
    /// The note's [`Note::info`]: every string key with a string value in its `userInfo`.
    pub info: BTreeMap<String, String>,
}

/// Where notifications go.
pub trait Notifier {
    /// Show `note`, replacing any up with its identifier.
    fn post(&self, note: Note);
    /// Take back the note with identifier `id`, shown or waiting.
    fn withdraw(&self, id: &str);
    /// Put `count` on the app's icon; zero clears it.
    fn set_badge(&self, count: usize);
}

/// A notifier that shows nothing and remembers what it was told: tests, and the self-test,
/// whose window is never in front and must not put banners on the person's screen.
#[derive(Debug, Default)]
pub struct Memory {
    posted: RefCell<Vec<Note>>,
    withdrawn: RefCell<Vec<String>>,
    badge: Cell<Option<usize>>,
}

impl Memory {
    /// Every note posted, oldest first.
    #[must_use]
    pub fn posted(&self) -> Vec<Note> {
        self.posted.borrow().clone()
    }

    /// Every identifier withdrawn, oldest first.
    #[must_use]
    pub fn withdrawn(&self) -> Vec<String> {
        self.withdrawn.borrow().clone()
    }

    /// The badge last set, if any was.
    #[must_use]
    pub const fn badge(&self) -> Option<usize> {
        self.badge.get()
    }

    /// Forget what was posted and withdrawn (the badge stays).
    pub fn clear(&self) {
        self.posted.borrow_mut().clear();
        self.withdrawn.borrow_mut().clear();
    }
}

impl Notifier for Memory {
    fn post(&self, note: Note) {
        self.posted.borrow_mut().push(note);
    }

    fn withdraw(&self, id: &str) {
        self.withdrawn.borrow_mut().push(id.to_owned());
    }

    fn set_badge(&self, count: usize) {
        self.badge.set(Some(count));
    }
}

#[cfg(target_vendor = "apple")]
pub use apple::{System, install, taps};
#[cfg(target_os = "ios")]
pub use ios::BackgroundGrace;

#[cfg(target_vendor = "apple")]
mod apple {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, ProtocolObject};
    use objc2::{AnyThread as _, define_class, msg_send};
    use objc2_foundation::{
        NSArray, NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSString,
    };
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
        UNNotification, UNNotificationPresentationOptions, UNNotificationRequest,
        UNNotificationResponse, UNNotificationSettings, UNNotificationSound,
        UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use parking_lot::Mutex;
    use tokio::sync::mpsc::error::SendError;
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

    use super::{Note, Notifier, Tap};

    /// Where the person's answer stands. Completion handlers write it from the framework's
    /// queues, so it sits behind a lock.
    #[derive(Debug)]
    enum Auth {
        /// Nothing posted yet this run.
        Unasked,
        /// The question is out; what was posted meanwhile, newest per identifier.
        Asking(Vec<Note>),
        /// Notes go out.
        Granted,
        /// Notes are dropped.
        Denied,
        /// No centre: the process is not an app bundle.
        Off,
    }

    #[derive(Debug)]
    struct State {
        auth: Auth,
        /// The icon badge last asked for (iOS: applied once notes are allowed).
        badge: Option<usize>,
    }

    /// Where taps go: held until the app listens, then down its channel.
    #[derive(Debug)]
    enum Inbox {
        /// Nobody listens (yet, or any more); oldest first.
        Held(Vec<Tap>),
        Listening(UnboundedSender<Tap>),
    }

    /// The process's one inbox: the delegate is installed before anything listens, and the tap
    /// that launched the app arrives in between. The delegate may run off the main thread.
    static INBOX: Mutex<Inbox> = Mutex::new(Inbox::Held(Vec::new()));

    /// Hand `tap` to the app, or hold it until the app listens.
    pub(super) fn deliver(tap: Tap) {
        let mut inbox = INBOX.lock();
        let tap = match &*inbox {
            Inbox::Listening(listener) => match listener.send(tap) {
                Ok(()) => return,
                Err(SendError(tap)) => tap,
            },
            Inbox::Held(_) => tap,
        };
        tracing::debug!(id = tap.id, "note opened while the app is not listening: held");
        match &mut *inbox {
            Inbox::Held(held) => held.push(tap),
            Inbox::Listening(_) => *inbox = Inbox::Held(vec![tap]),
        }
    }

    /// Every tap from now on, those held until now first, each once. A later call takes over:
    /// the tap after it goes to the newest listener.
    #[must_use]
    pub fn taps() -> UnboundedReceiver<Tap> {
        let (listener, taps) = tokio::sync::mpsc::unbounded_channel();
        let mut inbox = INBOX.lock();
        if let Inbox::Held(held) = &mut *inbox {
            for tap in held.drain(..) {
                // Cannot fail: the receiver is in hand.
                let _sent = listener.send(tap);
            }
        }
        *inbox = Inbox::Listening(listener);
        taps
    }

    /// Whether the process runs from an app bundle. Outside one (`cargo run`, a test binary)
    /// the centre raises `NSInternalInconsistencyException` ("bundleProxyForCurrentProcess is
    /// nil").
    fn in_bundle() -> bool {
        NSBundle::mainBundle().bundleIdentifier().is_some()
    }

    /// Install the notification centre's delegate, once per process; later calls do nothing.
    ///
    /// Call it before the app finishes launching. The SDK's header says of the tap callback:
    /// "The delegate must be set before the application returns from
    /// application:didFinishLaunchingWithOptions:". A tap that launched the app goes to no
    /// delegate installed after that. On iOS the UIKit shell calls this from its app delegate,
    /// because GPUI starts only once the scene connects; on macOS GPUI's run callback runs inside
    /// `applicationDidFinishLaunching:`, and [`System::new`] calls it there. Taps wait for
    /// [`taps`]. Nothing here prompts, and outside an app bundle nothing is installed.
    pub fn install() {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            if !in_bundle() {
                return;
            }
            let delegate = Delegate::new();
            UNUserNotificationCenter::currentNotificationCenter()
                .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            // The centre holds its delegate weakly, and this one serves the whole run.
            let _kept = Retained::into_raw(delegate);
        });
    }

    /// `UNUserNotificationCenter`, asked for authorisation on the first note.
    pub struct System {
        /// `None` outside an app bundle.
        center: Option<Retained<UNUserNotificationCenter>>,
        state: Arc<Mutex<State>>,
    }

    impl std::fmt::Debug for System {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("System").field("state", &self.state).finish_non_exhaustive()
        }
    }

    impl System {
        /// The notification centre with its delegate [installed](install): taps arrive on
        /// [`taps`], and a note that comes in while the app is in front goes to the list without
        /// a banner.
        ///
        /// A process that is not an app bundle gets no centre (`in_bundle`): its notes are
        /// dropped, and on macOS the Dock still shows the badge. Nothing here prompts; the
        /// settings are only read, so an app the person already allowed sets its badge before
        /// its first note.
        #[must_use]
        pub fn new() -> Self {
            if !in_bundle() {
                tracing::info!("notifications off: not an app bundle");
                let state = State { auth: Auth::Off, badge: None };
                return Self { center: None, state: Arc::new(Mutex::new(state)) };
            }
            install();
            let center = UNUserNotificationCenter::currentNotificationCenter();
            let state = Arc::new(Mutex::new(State { auth: Auth::Unasked, badge: None }));
            let read = Arc::clone(&state);
            let settings =
                RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
                    // SAFETY: UserNotifications rule: the settings handed to the completion handler
                    // are a valid object for the duration of the call.
                    let status = unsafe { settings.as_ref() }.authorizationStatus();
                    let allowed = [
                        UNAuthorizationStatus::Authorized,
                        UNAuthorizationStatus::Provisional,
                        UNAuthorizationStatus::Ephemeral,
                    ]
                    .contains(&status);
                    if !allowed {
                        return;
                    }
                    let badge = {
                        let mut state = read.lock();
                        if !matches!(state.auth, Auth::Unasked) {
                            return;
                        }
                        state.auth = Auth::Granted;
                        state.badge
                    };
                    if let Some(count) = badge {
                        icon_badge(count);
                    }
                });
            center.getNotificationSettingsWithCompletionHandler(&settings);
            Self { center: Some(center), state }
        }

        /// Ask for alerts, sounds and the badge, then send what waited if allowed.
        fn ask(&self) {
            let Some(center) = &self.center else { return };
            let state = Arc::clone(&self.state);
            let answered = RcBlock::new(move |granted: Bool, error: *mut NSError| {
                // SAFETY: UserNotifications rule: a non-null error is a valid `NSError` for the
                // duration of the completion handler.
                if let Some(error) = unsafe { error.as_ref() } {
                    tracing::warn!(error = %error.localizedDescription(), "notification authorisation");
                }
                let granted = granted.as_bool();
                tracing::info!(granted, "notification authorisation");
                let (waiting, badge) = {
                    let mut state = state.lock();
                    let before = std::mem::replace(
                        &mut state.auth,
                        if granted { Auth::Granted } else { Auth::Denied },
                    );
                    let waiting = match before {
                        Auth::Asking(waiting) => waiting,
                        Auth::Unasked | Auth::Granted | Auth::Denied | Auth::Off => Vec::new(),
                    };
                    (waiting, state.badge)
                };
                if !granted {
                    return;
                }
                let center = UNUserNotificationCenter::currentNotificationCenter();
                for note in &waiting {
                    add(&center, note);
                }
                if let Some(count) = badge {
                    icon_badge(count);
                }
            });
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert
                    | UNAuthorizationOptions::Sound
                    | UNAuthorizationOptions::Badge,
                &answered,
            );
        }
    }

    impl Default for System {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Notifier for System {
        fn post(&self, note: Note) {
            let ask = {
                let mut state = self.state.lock();
                match &mut state.auth {
                    Auth::Granted => {
                        if let Some(center) = &self.center {
                            add(center, &note);
                        }
                        false
                    }
                    Auth::Denied | Auth::Off => {
                        tracing::debug!(id = note.id, "note dropped: notifications are off");
                        false
                    }
                    Auth::Asking(waiting) => {
                        waiting.retain(|n| n.id != note.id);
                        waiting.push(note);
                        false
                    }
                    Auth::Unasked => {
                        state.auth = Auth::Asking(vec![note]);
                        true
                    }
                }
            };
            if ask {
                self.ask();
            }
        }

        fn withdraw(&self, id: &str) {
            if let Auth::Asking(waiting) = &mut self.state.lock().auth {
                waiting.retain(|n| n.id != id);
            }
            let Some(center) = &self.center else { return };
            let ids = NSArray::from_retained_slice(&[NSString::from_str(id)]);
            center.removePendingNotificationRequestsWithIdentifiers(&ids);
            center.removeDeliveredNotificationsWithIdentifiers(&ids);
        }

        fn set_badge(&self, count: usize) {
            let allowed = {
                let mut state = self.state.lock();
                state.badge = Some(count);
                matches!(state.auth, Auth::Granted)
            };
            // The Dock's badge needs no authorisation.
            if allowed || cfg!(target_os = "macos") {
                icon_badge(count);
            }
        }
    }

    /// The app icon's badge: the Dock's on macOS ([`crate::set_badge`]), the centre's on iOS,
    /// which shows only once notes are allowed.
    fn icon_badge(count: usize) {
        if cfg!(target_os = "macos") {
            crate::set_badge(count);
        } else {
            let count = isize::try_from(count).unwrap_or(isize::MAX);
            UNUserNotificationCenter::currentNotificationCenter()
                .setBadgeCount_withCompletionHandler(count, None);
        }
    }

    /// Hand `note` to the centre now: a nil trigger delivers at once, and the identifier
    /// replaces whatever is up under it.
    fn add(center: &UNUserNotificationCenter, note: &Note) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&note.title));
        content.setBody(&NSString::from_str(&note.body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        let keys: Vec<Retained<NSString>> =
            note.info.keys().map(|k| NSString::from_str(k)).collect();
        let values: Vec<Retained<NSString>> =
            note.info.values().map(|v| NSString::from_str(v)).collect();
        let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let values: Vec<&NSString> = values.iter().map(|v| &**v).collect();
        let info = NSDictionary::<NSString, NSString>::from_slices(&keys, &values);
        // SAFETY: `NSDictionary`'s generics are only a compile-time view; the object is the
        // same whatever they say, and `AnyObject` covers `NSString`.
        let info: &NSDictionary = unsafe { info.cast_unchecked() };
        // SAFETY: UserNotifications rule: `userInfo` holds property-list types only; every key
        // and value here is an `NSString`.
        unsafe {
            content.setUserInfo(info);
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&note.id),
            &content,
            None,
        );
        let id = note.id.clone();
        let added = RcBlock::new(move |error: *mut NSError| {
            // SAFETY: UserNotifications rule: a non-null error is a valid `NSError` for the
            // duration of the completion handler.
            if let Some(error) = unsafe { error.as_ref() } {
                tracing::warn!(id, error = %error.localizedDescription(), "note not delivered");
            }
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&added));
    }

    /// Every string key with a string value in a `userInfo` dictionary.
    fn strings(info: &NSDictionary) -> BTreeMap<String, String> {
        let (keys, values) = info.to_vecs();
        keys.iter()
            .zip(&values)
            .filter_map(|(k, v)| {
                let k = k.downcast_ref::<NSString>()?;
                let v = v.downcast_ref::<NSString>()?;
                Some((k.to_string(), v.to_string()))
            })
            .collect()
    }

    define_class!(
        // SAFETY:
        // - `NSObject` has no subclassing requirements.
        // - `Delegate` does not implement `Drop`.
        #[unsafe(super(NSObject))]
        #[name = "SloptyNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            /// The person opened a note, possibly off the main thread: its identifier and
            /// `userInfo` go to the app ([`deliver`]).
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                done: &block2::DynBlock<dyn Fn()>,
            ) {
                let request = response.notification().request();
                let tap = Tap {
                    id: request.identifier().to_string(),
                    info: strings(&request.content().userInfo()),
                };
                tracing::debug!(id = tap.id, "note opened");
                deliver(tap);
                done.call(());
            }

            /// A note that lands while the app is in front goes to the list only: the inbox
            /// already says it, and a banner over the app would say it twice.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                done: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                done.call((UNNotificationPresentationOptions::List,));
            }
        }
    );

    impl Delegate {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
            unsafe { msg_send![super(this), init] }
        }
    }
}

#[cfg(target_os = "ios")]
mod ios {
    use std::cell::Cell;
    use std::rc::Rc;

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2_foundation::NSString;
    use objc2_ui_kit::{UIApplication, UIBackgroundTaskIdentifier, UIBackgroundTaskInvalid};

    /// The time iOS grants an app that just left the screen, held from [`Self::begin`] until it
    /// is dropped or the system ends it.
    ///
    /// The system suspends the app when the grant runs out; the expiration handler gives the
    /// task back first, which the system requires of every background task.
    #[derive(Debug)]
    pub struct BackgroundGrace {
        task: Rc<Cell<UIBackgroundTaskIdentifier>>,
        mtm: MainThreadMarker,
    }

    impl BackgroundGrace {
        /// Ask for the grace, named `reason` for the system's diagnostics. Main thread only
        /// (UIKit); `None` off it, or when the system grants none.
        #[must_use]
        pub fn begin(reason: &str) -> Option<Self> {
            let mtm = MainThreadMarker::new()?;
            let app = UIApplication::sharedApplication(mtm);
            // SAFETY: UIKit's static, read once the framework is loaded, which linking it
            // guarantees.
            let invalid = unsafe { UIBackgroundTaskInvalid };
            let task = Rc::new(Cell::new(invalid));
            let expired = {
                let task = Rc::clone(&task);
                let app = app.clone();
                RcBlock::new(move || {
                    let id = task.replace(invalid);
                    if id != invalid {
                        tracing::info!("background grace ran out");
                        app.endBackgroundTask(id);
                    }
                })
            };
            let id = app.beginBackgroundTaskWithName_expirationHandler(
                Some(&NSString::from_str(reason)),
                Some(&expired),
            );
            if id == invalid {
                tracing::info!("no background grace granted");
                return None;
            }
            task.set(id);
            tracing::debug!(reason, "background grace");
            Some(Self { task, mtm })
        }
    }

    impl Drop for BackgroundGrace {
        fn drop(&mut self) {
            // SAFETY: as in `begin`.
            let invalid = unsafe { UIBackgroundTaskInvalid };
            let id = self.task.replace(invalid);
            if id != invalid {
                UIApplication::sharedApplication(self.mtm).endBackgroundTask(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_keeps_what_it_was_told_in_order() {
        let memory = Memory::default();
        memory.post(Note { id: "a".into(), title: "t".into(), ..Note::default() });
        memory.withdraw("a");
        memory.set_badge(3);
        assert_eq!(memory.posted().len(), 1, "one note posted");
        assert_eq!(memory.withdrawn(), vec!["a".to_owned()], "the note taken back");
        assert_eq!(memory.badge(), Some(3), "the badge last set");
        memory.clear();
        assert!(memory.posted().is_empty() && memory.withdrawn().is_empty(), "cleared");
        assert_eq!(memory.badge(), Some(3), "clearing keeps the badge");
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_tap_before_the_app_listens_arrives_once_it_does_exactly_once() {
        let tap = |id: &str| Tap { id: id.to_owned(), ..Tap::default() };
        apple::deliver(tap("launch"));
        let mut listening = taps();
        assert_eq!(listening.try_recv().ok(), Some(tap("launch")), "the held tap is handed over");
        assert!(listening.try_recv().is_err(), "and only once");
        apple::deliver(tap("later"));
        assert_eq!(
            listening.try_recv().ok(),
            Some(tap("later")),
            "a tap while listening goes through"
        );
        assert!(listening.try_recv().is_err(), "once");
        drop(listening);
        apple::deliver(tap("unheard"));
        let mut again = taps();
        assert_eq!(
            again.try_recv().ok(),
            Some(tap("unheard")),
            "a tap nobody heard waits for the next listener"
        );
        assert!(again.try_recv().is_err(), "with nothing replayed");
    }
}

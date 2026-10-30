//! Local notifications: a banner when a session in the background wants the human, the tap that
//! brings the app back where it points, and the icon badge.
//!
//! A note makes no sound: Slopty plays none (`docs/decisions/layout.md`, "Attention is an inbox
//! and a jump, never a feed").
//!
//! Everything goes through [`Notifier`]. What to say, and when, is decided elsewhere (the
//! workspace's `attention`) and tested against [`Memory`], so no test ever raises the system's
//! authorisation prompt or a banner. [`System`] is `UNUserNotificationCenter`, the same on macOS
//! and iOS. It asks for authorisation once, lazily: on the first note it posts, never before.
//! Making one only installs the delegate taps arrive through and reads the settings, and neither
//! prompts. A note posted while notes are off reads the settings again, so notes turned on in
//! System Settings go out from the next one; [`Notifier::alerts`] says how they stand, so the
//! app can say they are off rather than drop them unseen. [`settings`], [`ask`] and
//! [`open_settings`] are This Mac's checklist's: its Notifications line, its "Allow", and the
//! place to turn them on.
//!
//! The delegate is the process's, installed once by [`install`] while the app finishes launching:
//! the system hands a delegate installed any later no tap that launched the app. Taps wait in a
//! process-wide queue until the app listens ([`taps`]).
//!
//! A note carries no buttons: it answers nothing, and a tap opens the session it is about
//! (`docs/decisions/ui.md`, "A session is its terminal, its inspector beside it"). The centre's
//! categories are declared empty when [`System`] is made, which does not prompt either.
//!
//! `BackgroundGrace` keeps an iOS app running for the short time the system grants after it
//! leaves the screen, so the links stay up and what arrives just after the phone is pocketed
//! still notifies. Past it, the server pushes: a note sealed to the phone, which its
//! notification extension opens into the note the app would have posted.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

#[cfg(target_vendor = "apple")]
pub mod pushed;

/// The `userInfo` keys a note carries for its tap, the app's notes and pushed ones alike.
pub mod info {
    /// The worker the note is about, as the app keys it (its id's 128 bits, in decimal).
    pub const WORKER: &str = "worker";
    /// The tile's item.
    pub const ITEM: &str = "item";
    /// The terminal's session.
    pub const SESSION: &str = "session";
    /// The thread, for a note about one with no terminal.
    pub const THREAD: &str = "thread";
    /// The note's own identifier ([`super::Note::id`]). A pushed note is shown under the
    /// opaque collapse id it was pushed with, so this is how the app knows it as its own.
    pub const NOTE: &str = "note";
}

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
    /// Time Sensitive: an agent that needs the person, which breaks through a Focus and the
    /// notification summary. Nothing else is: the system shows the person how often an app
    /// uses it, and lets them take it away. The level wants its entitlement, which wants a
    /// provisioning profile; without one the system shows the note as an ordinary one.
    pub urgent: bool,
}

/// A note the system shows in its Notification Centre ([`delivered`]): one posted here, or
/// one the notification extension opened from a push.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Delivered {
    /// Its own identifier ([`Note::id`]), which a pushed note carries in [`info::NOTE`] as it
    /// is shown under its collapse id; [`Notifier::withdraw`] takes it back by this.
    pub id: String,
    /// The first line.
    pub title: String,
    /// What follows it.
    pub body: String,
}

/// A notification the human tapped.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Tap {
    /// The note's identifier.
    pub id: String,
    /// The note's [`Note::info`]: every string key with a string value in its `userInfo`.
    pub info: BTreeMap<String, String>,
}

/// Whether notes reach the person.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alerts {
    /// Not asked yet: the first note asks.
    Unasked,
    /// Allowed: notes show.
    Allowed,
    /// Turned off in the system's settings: notes are dropped.
    Denied,
    /// This process posts none: it is not an app bundle.
    Unavailable,
}

/// Where notifications go.
pub trait Notifier {
    /// Show `note`, replacing any up with its identifier.
    fn post(&self, note: Note);
    /// Take back the note with identifier `id`, shown or waiting.
    fn withdraw(&self, id: &str);
    /// Put `count` on the app's icon; zero clears it.
    fn set_badge(&self, count: usize);
    /// Whether a note posted now would reach the person, as last read.
    fn alerts(&self) -> Alerts;
}

/// A notifier that shows nothing and remembers what it was told: tests, and the self-test,
/// whose window is never in front and must not put banners on the person's screen.
#[derive(Debug)]
pub struct Memory {
    posted: RefCell<Vec<Note>>,
    withdrawn: RefCell<Vec<String>>,
    /// What the Notification Centre would show now, by identifier: posted and not taken back,
    /// or arrived by a push ([`Memory::push`]).
    delivered: RefCell<Vec<String>>,
    badge: Cell<Option<usize>>,
    alerts: Cell<Alerts>,
}

impl Default for Memory {
    /// Notes allowed, nothing posted yet.
    fn default() -> Self {
        Self {
            posted: RefCell::default(),
            withdrawn: RefCell::default(),
            delivered: RefCell::default(),
            badge: Cell::default(),
            alerts: Cell::new(Alerts::Allowed),
        }
    }
}

impl Memory {
    /// Say notes are `alerts` from now on, as the person's settings would.
    pub fn set_alerts(&self, alerts: Alerts) {
        self.alerts.set(alerts);
    }

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

    /// A note arrives by a push while the app is away, as the notification extension shows it:
    /// the system has it, though the app never posted it.
    pub fn push(&self, id: &str) {
        let mut delivered = self.delivered.borrow_mut();
        delivered.retain(|d| d != id);
        delivered.push(id.to_owned());
    }

    /// The identifiers of the notes shown now, oldest first, as the system lists its own.
    #[must_use]
    pub fn delivered(&self) -> Vec<String> {
        self.delivered.borrow().clone()
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
        self.push(&note.id);
        self.posted.borrow_mut().push(note);
    }

    fn withdraw(&self, id: &str) {
        self.delivered.borrow_mut().retain(|d| d != id);
        self.withdrawn.borrow_mut().push(id.to_owned());
    }

    fn set_badge(&self, count: usize) {
        self.badge.set(Some(count));
    }

    fn alerts(&self) -> Alerts {
        self.alerts.get()
    }
}

#[cfg(target_vendor = "apple")]
pub use apple::{
    System, ask, ask_quietly, content_of, delivered, install, open_settings, settings, take_back,
    taps,
};
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
        NSArray, NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSSet, NSString,
    };
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
        UNNotification, UNNotificationDefaultActionIdentifier, UNNotificationInterruptionLevel,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
        UNNotificationSettings, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use parking_lot::Mutex;
    use tokio::sync::mpsc::error::SendError;
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

    use super::{Alerts, Note, Notifier, Tap};

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

    /// The identifier the system shows a note under, by the note's own id, where the two
    /// differ: a pushed note is shown under its collapse id. Each listing ([`delivered`])
    /// replaces it, so it holds no more than the Notification Centre does.
    static SHOWN_AS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

    /// The note `request` is, by its own id ([`super::info::NOTE`]), else by the identifier
    /// it was shown under; and that identifier.
    fn ids_of(request: &UNNotificationRequest) -> (String, String) {
        let shown = request.identifier().to_string();
        let own = strings(&request.content().userInfo()).remove(super::info::NOTE);
        (own.unwrap_or_else(|| shown.clone()), shown)
    }

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
        /// The notification centre with its delegate [installed](install) and no categories: taps
        /// arrive on [`taps`], and a note that comes in while the app is in front goes to the
        /// list without a banner.
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
            // A note answers nothing, so no kind of note has buttons.
            center.setNotificationCategories(&NSSet::new());
            let state = Arc::new(Mutex::new(State { auth: Auth::Unasked, badge: None }));
            let read = Arc::clone(&state);
            read_settings(move |alerts| {
                let badge = {
                    let mut state = read.lock();
                    if !matches!(state.auth, Auth::Unasked) {
                        return;
                    }
                    match alerts {
                        Alerts::Allowed => state.auth = Auth::Granted,
                        Alerts::Denied => {
                            state.auth = Auth::Denied;
                            return;
                        }
                        Alerts::Unasked | Alerts::Unavailable => return,
                    }
                    state.badge
                };
                if let Some(count) = badge {
                    icon_badge(count);
                }
            });
            Self { center: Some(center), state }
        }

        /// Ask for alerts, sounds and the badge, then send what waited if allowed.
        fn ask(&self) {
            if self.center.is_none() {
                return;
            }
            let state = Arc::clone(&self.state);
            request(move |alerts| {
                let granted = alerts == Alerts::Allowed;
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
        }

        /// Notes were turned off when last read: read again, since the person may have turned
        /// them on in System Settings since, and send `note` if they did.
        fn reread(&self, note: Note) {
            let state = Arc::clone(&self.state);
            read_settings(move |alerts| {
                if alerts != Alerts::Allowed {
                    tracing::debug!(id = note.id, "note dropped: notifications are off");
                    return;
                }
                let badge = {
                    let mut state = state.lock();
                    state.auth = Auth::Granted;
                    state.badge
                };
                add(&UNUserNotificationCenter::currentNotificationCenter(), &note);
                if let Some(count) = badge {
                    icon_badge(count);
                }
            });
        }
    }

    /// Read the centre's settings and hand what they say to `then`, on the framework's queue.
    /// Nothing prompts.
    fn read_settings(then: impl Fn(Alerts) + 'static) {
        let read = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
            // SAFETY: UserNotifications rule: the settings handed to the completion handler are
            // a valid object for the duration of the call.
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            then(alerts_of(status));
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&read);
    }

    /// The options the app asks for: alerts and the badge, no sound.
    const ASKED: UNAuthorizationOptions =
        UNAuthorizationOptions::Alert.union(UNAuthorizationOptions::Badge);

    /// Ask for alerts and the badge (the system's prompt, which shows once ever) and
    /// hand the answer to `then`, on the framework's queue.
    fn request(then: impl Fn(Alerts) + 'static) {
        request_with(ASKED, then);
    }

    /// Ask for `options`, handing the answer to `then` on the framework's queue.
    fn request_with(options: UNAuthorizationOptions, then: impl Fn(Alerts) + 'static) {
        let answered = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            // SAFETY: UserNotifications rule: a non-null error is a valid `NSError` for the
            // duration of the completion handler.
            if let Some(error) = unsafe { error.as_ref() } {
                tracing::warn!(error = %error.localizedDescription(), "notification authorisation");
            }
            let granted = granted.as_bool();
            tracing::info!(granted, "notification authorisation");
            then(if granted { Alerts::Allowed } else { Alerts::Denied });
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(options, &answered);
    }

    /// What an authorisation status means for a note.
    fn alerts_of(status: UNAuthorizationStatus) -> Alerts {
        if status == UNAuthorizationStatus::NotDetermined {
            Alerts::Unasked
        } else if [
            UNAuthorizationStatus::Authorized,
            UNAuthorizationStatus::Provisional,
            UNAuthorizationStatus::Ephemeral,
        ]
        .contains(&status)
        {
            Alerts::Allowed
        } else {
            Alerts::Denied
        }
    }

    /// The first answer a framework's queue hands on, as a future: a completion handler may be
    /// called from any thread, and is called once.
    fn answer() -> (impl Fn(Alerts) + 'static, tokio::sync::oneshot::Receiver<Alerts>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Mutex::new(Some(tx));
        let send = move |alerts| {
            let tx = tx.lock().take();
            if let Some(tx) = tx {
                let _unheard = tx.send(alerts);
            }
        };
        (send, rx)
    }

    /// Whether notes reach the person, as the system's settings say now: what This Mac's
    /// checklist shows. Nothing prompts.
    pub async fn settings() -> Alerts {
        if !in_bundle() {
            return Alerts::Unavailable;
        }
        let (send, answered) = answer();
        read_settings(send);
        answered.await.unwrap_or(Alerts::Unasked)
    }

    /// Ask the person for notes now (the system's prompt, which shows once ever), and say how
    /// they stand after: the checklist's "Allow".
    pub async fn ask() -> Alerts {
        if !in_bundle() {
            return Alerts::Unavailable;
        }
        let (send, answered) = answer();
        request(send);
        answered.await.unwrap_or(Alerts::Unasked)
    }

    /// Ask for notes with no prompt: provisional authorisation.
    ///
    /// Its notes go quietly to the Notification Centre until the person keeps or turns them
    /// off there. The self-test's simulator has no person to answer the prompt [`ask`] raises,
    /// and `simctl` cannot grant notes; the app itself never asks this way.
    pub async fn ask_quietly() -> Alerts {
        if !in_bundle() {
            return Alerts::Unavailable;
        }
        let (send, answered) = answer();
        request_with(ASKED | UNAuthorizationOptions::Provisional, send);
        answered.await.unwrap_or(Alerts::Unasked)
    }

    /// The notes the Notification Centre shows for this app now, newest first as the system
    /// lists them; none outside an app bundle.
    pub async fn delivered() -> Vec<super::Delivered> {
        if !in_bundle() {
            return Vec::new();
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Mutex::new(Some(tx));
        let listed = RcBlock::new(move |notes: std::ptr::NonNull<NSArray<UNNotification>>| {
            // SAFETY: UserNotifications rule: the array handed to the completion handler is a
            // valid object for the duration of the call.
            let notes = unsafe { notes.as_ref() };
            let mut shown_as = BTreeMap::new();
            let notes: Vec<super::Delivered> = notes
                .iter()
                .map(|note| {
                    let request = note.request();
                    let content = request.content();
                    let (id, shown) = ids_of(&request);
                    if id != shown {
                        shown_as.insert(id.clone(), shown);
                    }
                    super::Delivered {
                        id,
                        title: content.title().to_string(),
                        body: content.body().to_string(),
                    }
                })
                .collect();
            *SHOWN_AS.lock() = shown_as;
            let tx = tx.lock().take();
            if let Some(tx) = tx {
                let _unheard = tx.send(notes);
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getDeliveredNotificationsWithCompletionHandler(&listed);
        rx.await.unwrap_or_default()
    }

    /// Take down the notes shown under `ids`, then call `done`: a background push's take-back
    /// ([`super::pushed::taken_back`]), while the app has only the moments iOS gives it.
    ///
    /// The removal goes out first, then a listing is asked, and `done` waits for its answer,
    /// so the app is not suspended with the removal still unsent. Outside an app bundle, or
    /// with nothing to take down, `done` runs at once.
    pub fn take_back(ids: &[String], done: impl FnOnce() + Send + 'static) {
        if ids.is_empty() || !in_bundle() {
            done();
            return;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let ids: Vec<Retained<NSString>> = ids.iter().map(|id| NSString::from_str(id)).collect();
        center.removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(&ids));
        let done = Mutex::new(Some(done));
        let listed = RcBlock::new(move |_notes: std::ptr::NonNull<NSArray<UNNotification>>| {
            let done = done.lock().take();
            if let Some(done) = done {
                done();
            }
        });
        center.getDeliveredNotificationsWithCompletionHandler(&listed);
    }

    /// Open the system's settings at Slopty's notifications, where the person turns them on.
    pub fn open_settings() {
        #[cfg(target_os = "macos")]
        {
            let Some(id) = NSBundle::mainBundle().bundleIdentifier() else { return };
            crate::open_url(&format!(
                "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={id}"
            ));
        }
        #[cfg(target_os = "ios")]
        {
            // SAFETY: UIKit rule: the constant is a string UIKit exports, valid once it is
            // loaded, which linking it guarantees.
            let url = unsafe { objc2_ui_kit::UIApplicationOpenNotificationSettingsURLString };
            crate::open_url(&url.to_string());
        }
    }

    impl Default for System {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Notifier for System {
        fn post(&self, note: Note) {
            let mut reread = None;
            let ask = {
                let mut state = self.state.lock();
                match &mut state.auth {
                    Auth::Granted => {
                        if let Some(center) = &self.center {
                            add(center, &note);
                        }
                        false
                    }
                    Auth::Denied => {
                        reread = Some(note);
                        false
                    }
                    Auth::Off => {
                        tracing::debug!(id = note.id, "note dropped: not an app bundle");
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
            if let Some(note) = reread {
                self.reread(note);
            }
        }

        fn withdraw(&self, id: &str) {
            if let Auth::Asking(waiting) = &mut self.state.lock().auth {
                waiting.retain(|n| n.id != id);
            }
            let Some(center) = &self.center else { return };
            let shown = SHOWN_AS.lock().remove(id);
            let ids: Vec<Retained<NSString>> =
                std::iter::once(id).chain(shown.as_deref()).map(NSString::from_str).collect();
            let ids = NSArray::from_retained_slice(&ids);
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

        fn alerts(&self) -> Alerts {
            match self.state.lock().auth {
                Auth::Unasked | Auth::Asking(_) => Alerts::Unasked,
                Auth::Granted => Alerts::Allowed,
                Auth::Denied => Alerts::Denied,
                Auth::Off => Alerts::Unavailable,
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
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&note.id),
            &content_of(note),
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

    /// `note` as the system shows it: what the app posts, and what the notification extension
    /// hands back for a push.
    pub fn content_of(note: &Note) -> Retained<UNMutableNotificationContent> {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&note.title));
        content.setBody(&NSString::from_str(&note.body));
        if note.urgent {
            content.setInterruptionLevel(UNNotificationInterruptionLevel::TimeSensitive);
        }
        let mut info = note.info.clone();
        info.insert(super::info::NOTE.to_owned(), note.id.clone());
        let keys: Vec<Retained<NSString>> = info.keys().map(|k| NSString::from_str(k)).collect();
        let values: Vec<Retained<NSString>> =
            info.values().map(|v| NSString::from_str(v)).collect();
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
        content
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
            /// `userInfo` go to the app ([`deliver`]). Only the note itself opens: a note has no
            /// buttons, and one swept away is nothing to hand on.
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                done: &block2::DynBlock<dyn Fn()>,
            ) {
                // SAFETY: UserNotifications rule: the action identifier is a constant string the
                // framework exports, valid once it is loaded, which linking it guarantees.
                let opened = unsafe { UNNotificationDefaultActionIdentifier };
                if response.actionIdentifier().isEqualToString(opened) {
                    let request = response.notification().request();
                    let (id, shown) = ids_of(&request);
                    if id != shown {
                        SHOWN_AS.lock().insert(id.clone(), shown);
                    }
                    tracing::debug!(id, "note opened");
                    deliver(Tap { id, info: strings(&request.content().userInfo()) });
                }
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

    /// What the Notification Centre shows is kept as the system keeps it: a note posted, or
    /// arrived by a push, until it is taken back; one posted again under its identifier is shown
    /// once.
    #[test]
    fn memory_keeps_what_the_notification_centre_shows() {
        let memory = Memory::default();
        memory.post(Note { id: "a".into(), ..Note::default() });
        memory.push("b");
        memory.post(Note { id: "a".into(), ..Note::default() });
        assert_eq!(memory.delivered(), ["b", "a"]);
        memory.withdraw("b");
        assert_eq!(memory.delivered(), ["a"]);
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

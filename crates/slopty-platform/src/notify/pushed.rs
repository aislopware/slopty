//! A note pushed to a pocketed phone (`docs/decisions/platform.md`, "Notes reach a pocketed
//! phone").
//!
//! Past the background grace, the server seals the notice it would have sent this client to a
//! key only the phone holds ([`slopty_proto::push::PushBody`]) and pushes it through APNs. The
//! notification extension opens it ([`opened`]) into the note the app would have posted
//! ([`note_of`]), which the system shows as the app's own ([`super::content_of`]); a tap routes
//! as one on a local note does, since it carries the same [`super::info`] keys.
//!
//! A note answered elsewhere is taken back by a background push naming the ids it was shown
//! under ([`taken_back`]), which the app delegate hands to [`super::take_back`].
//!
//! The phone's side of the registration lives here too: the device token APNs gives the app
//! ([`token_arrived`], [`tokens`]), and, on iOS, the key and the token kept in the Keychain
//! where the extension reads them (`device_key`, `stored`, iOS only).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use slopty_proto::push::PushBody;
use slopty_proto::thread::attention::{Notice, NoticeKind, Subject};
use slopty_push::seal::{DeviceKey, Sealed};
use tokio::sync::watch;

use super::{APPROVAL, Note, info};

/// The `userInfo` key of a push's encapsulated key, as APNs delivers it.
pub const ENC: &str = "e";
/// The `userInfo` key of a push's ciphertext.
pub const SEALED: &str = "s";

/// What a notice says in a note's body: its text, named by the subagent it came from.
#[must_use]
pub fn notice_body(notice: &Notice) -> String {
    match &notice.via {
        Some(via) if !notice.text.is_empty() => format!("{}: {}", via.title, notice.text),
        Some(via) => via.title.clone(),
        None => notice.text.clone(),
    }
}

/// The identifier of a note about `notice`.
///
/// It is its terminal's session (a thread's tile, or a program's own terminal), else its
/// thread's, else its project's timeline entry. The app's note about the same moment has the
/// same one, so either replaces the other.
#[must_use]
pub fn note_id(notice: &Notice) -> String {
    match (&notice.about, notice.tile) {
        (Subject::Project { project, entry }, _) => format!("project-{project}-{entry}"),
        (Subject::Thread(_), Some(tile)) => tile.session.to_string(),
        (Subject::Thread(at), None) => format!("{}-{}", info::THREAD, at.thread),
        (Subject::Terminal(term), _) => term.session.to_string(),
    }
}

/// The note `body` makes on the phone: what the app would have posted for its notice.
///
/// It has a question's options as its buttons when the body offers them, the approval's when it
/// carries a yes or no they answer, else, for an agent's thread, a reply ([`super::REPLYING`]). A
/// quiet push only moves what its buttons answer under a note already up, so it sounds no second
/// time.
///
/// It knows no tile, so it carries no [`info::ITEM`]; the app finds the tile by the session or
/// the thread. A thread's note with no title says the app's name, which stands in for the tile
/// the app would have named.
#[must_use]
pub fn note_of(body: &PushBody) -> Note {
    let notice = &body.notice;
    let mut keys = BTreeMap::new();
    let worker = match (&notice.about, notice.tile) {
        (Subject::Thread(at), _) => Some(at.worker),
        (Subject::Terminal(term), _) => Some(term.worker),
        (Subject::Project { .. }, tile) => tile.map(|t| t.worker),
    };
    if let Some(worker) = worker {
        keys.insert(info::WORKER.to_owned(), worker.as_uuid().as_u128().to_string());
    }
    match (&notice.about, notice.tile) {
        (_, Some(tile)) => {
            keys.insert(info::SESSION.to_owned(), tile.session.to_string());
        }
        (Subject::Thread(at), None) => {
            keys.insert(info::THREAD.to_owned(), at.thread.to_string());
        }
        (Subject::Terminal(term), None) => {
            keys.insert(info::SESSION.to_owned(), term.session.to_string());
        }
        (Subject::Project { .. }, None) => {}
    }
    if let Some(ask) = &body.ask {
        keys.insert(info::ASK.to_owned(), ask.0.clone());
    }
    if let (Some(task), Subject::Project { project, .. }) = (body.merges, &notice.about) {
        keys.insert(info::PROJECT.to_owned(), project.to_string());
        keys.insert(info::TASK.to_owned(), task.0.to_string());
    }
    let title = Some(notice.title.trim())
        .filter(|t| !t.is_empty())
        .map_or_else(|| slopty_push::apns::TITLE.to_owned(), str::to_owned);
    let thread = match &notice.about {
        Subject::Project { project, .. } => Some(project.as_str().to_owned()),
        Subject::Thread(_) | Subject::Terminal(_) => None,
    };
    let picks = if body.ask.is_some() { body.choices.as_slice() } else { &[] };
    Note {
        id: note_id(notice),
        title,
        body: notice_body(notice),
        info: keys,
        thread,
        category: category_of(body),
        picks: Vec::new(),
        silent: body.quiet,
        urgent: notice.kind == NoticeKind::NeedsYou,
    }
    .picking(picks)
}

/// The buttons a pushed note carries: a question's options are its picks ([`Note::picking`]),
/// which take a category of their own; else Allow and Deny on a yes or no, else a reply on an
/// agent's own moment (one that needs the person, failed or finished), else Merge on a
/// project's work ready to merge that names the task to merge ([`PushBody::merges`]). A
/// program's record and a project's other notices have none.
fn category_of(body: &PushBody) -> Option<super::Category> {
    if body.ask.is_some() && !body.choices.is_empty() {
        return None;
    }
    if body.merges.is_some() && matches!(body.notice.about, Subject::Project { .. }) {
        return Some(super::MERGING);
    }
    if body.ask.is_some() {
        return Some(APPROVAL);
    }
    let agent = matches!(body.notice.about, Subject::Thread(_));
    let moment = matches!(
        body.notice.kind,
        NoticeKind::NeedsYou | NoticeKind::Failed | NoticeKind::Finished
    );
    (agent && moment).then_some(super::REPLYING)
}

/// The notes a background push takes back, from the ids under its
/// [`slopty_push::apns::TAKE_BACK`] key as `list` gives them.
///
/// Each is an opaque id as the server makes them, at most
/// [`slopty_push::apns::MAX_TAKE_BACK`]. Anything else in it is passed over.
#[must_use]
pub fn taken_back(list: impl IntoIterator<Item = String>) -> Vec<String> {
    list.into_iter()
        .filter(|id| slopty_push::apns::is_id(id))
        .take(slopty_push::apns::MAX_TAKE_BACK)
        .collect()
}

/// The ids a background push's `userInfo` names to take back ([`taken_back`]); none when it
/// is no take-back.
#[cfg(target_os = "ios")]
#[must_use]
pub fn take_back_of(info: &objc2_foundation::NSDictionary) -> Vec<String> {
    use objc2_foundation::{NSArray, NSString};
    let Some(list) = info.objectForKey(&NSString::from_str(slopty_push::apns::TAKE_BACK)) else {
        return Vec::new();
    };
    let Ok(list) = list.downcast::<NSArray>() else { return Vec::new() };
    taken_back(list.iter().filter_map(|id| id.downcast::<NSString>().ok().map(|id| id.to_string())))
}

/// Why a push did not open.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// It carries no sealed body, or one that is not base64.
    #[error("no sealed body")]
    NotSealed,
    /// It did not open with this phone's key under its token.
    #[error("{0}")]
    Seal(#[from] slopty_push::PushError),
    /// It opened to something that is not a body.
    #[error("{0}")]
    Decode(#[from] slopty_proto::codec::CodecError),
}

/// The note a push carrying `enc` and `sealed` makes, opened with this phone's `key` under its
/// `token`.
///
/// # Errors
/// [`OpenError`] when the push carries no body, is not this phone's, or opens to no body.
pub fn opened(enc: &str, sealed: &str, token: &str, key: &DeviceKey) -> Result<Note, OpenError> {
    let sealed = Sealed::from_text(enc, sealed).ok_or(OpenError::NotSealed)?;
    let bytes = key.open(token, &sealed)?;
    let body: PushBody = slopty_proto::codec::decode_body(&bytes)?;
    Ok(note_of(&body))
}

/// The device token APNs last gave this app, in hex; `None` until it gave one.
static TOKEN: LazyLock<watch::Sender<Option<String>>> = LazyLock::new(|| watch::Sender::new(None));

/// APNs gave the app `token` (the app delegate's `didRegisterForRemoteNotifications`): kept
/// for the extension (`remember_token`, iOS only) and handed to whoever follows [`tokens`].
pub fn token_arrived(token: &[u8]) {
    let hex = token.iter().fold(String::new(), |mut hex, b| {
        use std::fmt::Write as _;
        let _infallible = write!(hex, "{b:02x}");
        hex
    });
    #[cfg(target_os = "ios")]
    if let Err(e) = keychain::put(keychain::TOKEN, hex.as_bytes()) {
        tracing::warn!(error = %e, "the device token was not kept for the extension");
    }
    TOKEN.send_if_modified(|now| {
        let changed = now.as_deref() != Some(hex.as_str());
        *now = Some(hex.clone());
        changed
    });
}

/// The device token, as it arrives and each time APNs changes it.
#[must_use]
pub fn tokens() -> watch::Receiver<Option<String>> {
    TOKEN.subscribe()
}

/// Ask APNs for this app's device token: it arrives through the app delegate
/// ([`token_arrived`]). It never prompts; whether a note shows is the notes' own permission.
#[cfg(target_os = "ios")]
pub fn register() {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        tracing::warn!("registering for pushes off the main thread");
        return;
    };
    objc2_ui_kit::UIApplication::sharedApplication(mtm).registerForRemoteNotifications();
}

/// This phone's key, made on first use and kept in the Keychain, readable once the phone has
/// been unlocked since it started, never backed up, and shared with the extension.
///
/// # Errors
/// [`keychain::KeychainError`] when it can't be read or kept, or no key could be made.
#[cfg(target_os = "ios")]
pub fn device_key() -> Result<DeviceKey, keychain::KeychainError> {
    if let Some(bytes) = keychain::get(keychain::KEY)? {
        if let Ok(key) = DeviceKey::from_bytes(&bytes) {
            return Ok(key);
        }
        tracing::warn!("the kept push key is not one; making another");
    }
    let key = DeviceKey::generate().map_err(|_random| keychain::KeychainError::Random)?;
    keychain::put(keychain::KEY, &key.to_bytes())?;
    Ok(key)
}

/// The key and the device token the app kept, as the extension reads them; `None` before the
/// app made both.
///
/// # Errors
/// [`keychain::KeychainError`] when the Keychain can't be read: locked since the phone
/// started, or not shared with this process.
#[cfg(target_os = "ios")]
pub fn stored() -> Result<Option<(DeviceKey, String)>, keychain::KeychainError> {
    let (Some(key), Some(token)) = (keychain::get(keychain::KEY)?, keychain::get(keychain::TOKEN)?)
    else {
        return Ok(None);
    };
    let key = DeviceKey::from_bytes(&key).map_err(|_bad| keychain::KeychainError::Corrupt)?;
    let token = String::from_utf8(token).map_err(|_bad| keychain::KeychainError::Corrupt)?;
    Ok(Some((key, token)))
}

/// Why a push stays as it came, rather than opening into its note.
#[cfg(target_os = "ios")]
#[derive(Debug, thiserror::Error)]
pub enum Kept {
    /// It carries no sealed body.
    #[error("no sealed body in the push")]
    Unsealed,
    /// The key or the token is not there yet.
    #[error("the app has kept no key and token yet")]
    NoKey,
    /// The Keychain did not answer.
    #[error("{0}")]
    Keychain(#[from] keychain::KeychainError),
    /// It did not open.
    #[error("{0}")]
    Open(#[from] OpenError),
}

/// The note a push opens to, its `userInfo` read through `text`, with the key and the token
/// the app kept.
///
/// It is the notification extension's whole work, and the self-test's
/// (`docs/decisions/platform.md`, "Notes reach a pocketed phone").
///
/// # Errors
/// [`Kept`] when the push carries no sealed body, the app kept no key and token yet, the
/// Keychain does not answer, or the body does not open.
#[cfg(target_os = "ios")]
pub fn note_from(text: impl Fn(&str) -> Option<String>) -> Result<Note, Kept> {
    let (Some(enc), Some(sealed)) = (text(ENC), text(SEALED)) else {
        return Err(Kept::Unsealed);
    };
    let (key, token) = stored()?.ok_or(Kept::NoKey)?;
    Ok(opened(&enc, &sealed, &token, &key)?)
}

#[cfg(target_os = "ios")]
pub mod keychain {
    //! The push's two items in the Keychain: generic passwords under one service.
    //!
    //! They sit in the access group the app and its extension share: their entitlements' first,
    //! which is where an item goes when none is named.

    use objc2_core_foundation::{CFData, CFDictionary, CFRetained, CFString, CFType};
    use objc2_security::{
        SecItemAdd, SecItemCopyMatching, SecItemDelete, errSecItemNotFound, errSecSuccess,
        kSecAttrAccessible, kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly, kSecAttrAccount,
        kSecAttrService, kSecClass, kSecClassGenericPassword, kSecMatchLimit, kSecMatchLimitOne,
        kSecReturnData, kSecValueData,
    };

    /// The service both items sit under.
    const SERVICE: &str = "dev.aislopware.slopty.push";
    /// The phone's private key.
    pub const KEY: &str = "key";
    /// The device token, in hex.
    pub const TOKEN: &str = "token";

    /// Why the Keychain did not answer.
    #[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
    pub enum KeychainError {
        /// The Keychain said no, with its status.
        #[error("the keychain said {0}")]
        Status(i32),
        /// An item is not what was kept.
        #[error("a kept item is not what it should be")]
        Corrupt,
        /// No key could be made: the random source failed.
        #[error("no random source")]
        Random,
    }

    /// The query for item `account`.
    fn query(
        account: &str,
        extra: &[(&CFString, &CFType)],
    ) -> CFRetained<CFDictionary<CFString, CFType>> {
        let service = CFString::from_str(SERVICE);
        let account = CFString::from_str(account);
        // SAFETY: Security's constant strings, read once the framework is loaded, which linking
        // it guarantees.
        let (class, generic, service_key, account_key) =
            unsafe { (kSecClass, kSecClassGenericPassword, kSecAttrService, kSecAttrAccount) };
        let mut keys: Vec<&CFString> = vec![class, service_key, account_key];
        let mut values: Vec<&CFType> = vec![generic.as_ref(), service.as_ref(), account.as_ref()];
        for (key, value) in extra {
            keys.push(key);
            values.push(value);
        }
        CFDictionary::from_slices(&keys, &values)
    }

    /// What item `account` holds; `None` when there is none.
    ///
    /// # Errors
    /// [`KeychainError::Status`] when the Keychain can't be read.
    pub fn get(account: &str) -> Result<Option<Vec<u8>>, KeychainError> {
        // SAFETY: as in `query`.
        let (return_data, limit, one) =
            unsafe { (kSecReturnData, kSecMatchLimit, kSecMatchLimitOne) };
        let yes = objc2_core_foundation::CFBoolean::new(true);
        let query = query(account, &[(return_data, yes.as_ref()), (limit, one.as_ref())]);
        let mut found: *const CFType = std::ptr::null();
        // SAFETY: Security's rule for `SecItemCopyMatching`: a dictionary of CFString keys and a
        // valid out-pointer, which on success holds a +1 reference the caller releases.
        let status = unsafe { SecItemCopyMatching(query.as_opaque(), &raw mut found) };
        if status == errSecItemNotFound {
            return Ok(None);
        }
        if status != errSecSuccess {
            return Err(KeychainError::Status(status));
        }
        let Some(found) = std::ptr::NonNull::new(found.cast_mut()) else {
            return Err(KeychainError::Corrupt);
        };
        // SAFETY: the +1 reference `SecItemCopyMatching` returned, taken over once.
        let found: CFRetained<CFType> = unsafe { CFRetained::from_raw(found) };
        let data = found.downcast::<CFData>().map_err(|_other| KeychainError::Corrupt)?;
        Ok(Some(data.to_vec()))
    }

    /// Keep `bytes` as item `account`, replacing what it held, readable once the phone has
    /// been unlocked since it started, and never leaving it.
    ///
    /// # Errors
    /// [`KeychainError::Status`] when the Keychain can't be written.
    pub fn put(account: &str, bytes: &[u8]) -> Result<(), KeychainError> {
        let gone = query(account, &[]);
        // SAFETY: Security's rule for `SecItemDelete`: a dictionary of CFString keys.
        let status = unsafe { SecItemDelete(gone.as_opaque()) };
        if status != errSecSuccess && status != errSecItemNotFound {
            return Err(KeychainError::Status(status));
        }
        let data = CFData::from_bytes(bytes);
        // SAFETY: as in `query`.
        let (value, accessible, after_first_unlock) = unsafe {
            (kSecValueData, kSecAttrAccessible, kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly)
        };
        let item =
            query(account, &[(value, data.as_ref()), (accessible, after_first_unlock.as_ref())]);
        // SAFETY: Security's rule for `SecItemAdd`: a dictionary of CFString keys, and a null
        // result pointer when no result is wanted.
        let status = unsafe { SecItemAdd(item.as_opaque(), std::ptr::null_mut()) };
        if status != errSecSuccess {
            return Err(KeychainError::Status(status));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::{SessionId, WorkerId};
    use slopty_proto::orchestration::TermRef;
    use slopty_proto::project::ProjectId;
    use slopty_proto::thread::attention::{ThreadAt, Via};
    use slopty_proto::thread::{AskId, ThreadId};

    use super::*;

    fn notice(kind: NoticeKind, about: Subject, tile: Option<TermRef>) -> Notice {
        Notice {
            kind,
            about,
            tile,
            title: "Rank the fleet".to_owned(),
            text: "Run cargo test?".to_owned(),
            worked_ms: None,
            via: None,
        }
    }

    /// Work ready to merge carries "Merge" with the project and the task it merges, which a
    /// press hands back to merge with no round trip; the same notice naming no task, or a
    /// thread's note, has no Merge.
    #[test]
    fn a_ready_note_merges_the_task_it_names() {
        let project = ProjectId::new("store").expect("a name");
        let about = Subject::Project { project: project.clone(), entry: 7 };
        let body = PushBody {
            notice: notice(NoticeKind::ReadyToMerge, about, None),
            ask: None,
            choices: Vec::new(),
            quiet: false,
            merges: Some(slopty_proto::project::TaskId(4)),
        };
        let note = note_of(&body);
        assert_eq!(note.category, Some(super::super::MERGING));
        assert_eq!(note.info.get(info::PROJECT), Some(&"store".to_owned()));
        assert_eq!(note.info.get(info::TASK), Some(&"4".to_owned()));
        let tap = super::super::Tap {
            id: note.id.clone(),
            info: note.info,
            action: Some(super::super::MERGE.to_owned()),
            text: None,
        };
        assert_eq!(
            super::super::merge_of(&tap),
            Some((project, slopty_proto::project::TaskId(4))),
            "a press merges the task the note names"
        );
        assert!(tap.finished_later(), "answered where the note is");
        let shown = super::super::Tap { action: Some(super::super::SHOW.to_owned()), ..tap };
        assert_eq!(super::super::merge_of(&shown), None, "Show only opens");

        let unnamed = note_of(&PushBody { merges: None, ..body });
        assert_eq!(unnamed.category, None, "no task, no Merge");
        assert!(!unnamed.info.contains_key(info::TASK));
    }

    /// A thread at a terminal is that terminal's note, with its worker and session for the
    /// tap, urgent and with the buttons when it asks a yes or no; one with no terminal is its
    /// thread's, and a project's is its timeline entry's, stacked under the project.
    #[test]
    fn a_pushed_note_carries_what_a_tap_routes_by() {
        let (worker, session, thread) = (WorkerId::new(), SessionId::new(), ThreadId::new());
        let at = Subject::Thread(ThreadAt { worker, thread });
        let tile = TermRef { worker, session };
        let ask = AskId("toolu_01".to_owned());
        let body = PushBody {
            notice: notice(NoticeKind::NeedsYou, at.clone(), Some(tile)),
            ask: Some(ask),
            choices: Vec::new(),
            quiet: false,
            merges: None,
        };
        let note = note_of(&body);
        assert_eq!(note.id, session.to_string());
        assert_eq!(
            (note.title.as_str(), note.body.as_str()),
            ("Rank the fleet", "Run cargo test?")
        );
        let keys = |pairs: &[(&str, String)]| -> BTreeMap<String, String> {
            pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
        };
        let worker_key = worker.as_uuid().as_u128().to_string();
        assert_eq!(
            note.info,
            keys(&[
                (info::WORKER, worker_key.clone()),
                (info::SESSION, session.to_string()),
                (info::ASK, "toolu_01".to_owned()),
            ])
        );
        assert_eq!((note.category, note.urgent, note.thread), (Some(APPROVAL), true, None));
        assert!(!note.silent, "the first push sounds");
        let moved = note_of(&PushBody { ask: None, choices: Vec::new(), quiet: true, ..body });
        assert_eq!(moved.id, note.id, "in place of the note up");
        assert!(moved.silent, "a quiet push only moves its buttons");
        assert_eq!(moved.category, Some(super::super::REPLYING), "the yes or no went: a reply");

        let mut done = notice(NoticeKind::Finished, at, None);
        done.via = Some(Via { thread: ThreadId::new(), title: "Explore".to_owned() });
        done.title = " ".to_owned();
        let note = note_of(&PushBody {
            notice: done,
            ask: None,
            choices: Vec::new(),
            quiet: false,
            merges: None,
        });
        assert_eq!(note.id, format!("thread-{thread}"));
        assert_eq!(
            note.info,
            keys(&[(info::WORKER, worker_key), (info::THREAD, thread.to_string())])
        );
        assert_eq!(
            (note.title.as_str(), note.body.as_str()),
            ("Slopty", "Explore: Run cargo test?")
        );
        assert_eq!((note.category, note.urgent), (Some(super::super::REPLYING), false));

        let project = ProjectId::new("ladder").unwrap();
        let about = Subject::Project { project: project.clone(), entry: 7 };
        let note = note_of(&PushBody {
            notice: notice(NoticeKind::Project, about, Some(tile)),
            ask: None,
            choices: Vec::new(),
            quiet: false,
            merges: None,
        });
        assert_eq!(note.id, format!("project-{project}-7"));
        assert_eq!(note.thread.as_deref(), Some("ladder"));
        assert_eq!(note.category, None, "a project's notice takes no reply");
        assert_eq!(note.info.get(info::SESSION), Some(&session.to_string()));

        // A program's own wait leads to its terminal, under the id the app's own note has.
        let program = notice(NoticeKind::NeedsYou, Subject::Terminal(tile), None);
        let note = note_of(&PushBody {
            notice: program,
            ask: None,
            choices: Vec::new(),
            quiet: false,
            merges: None,
        });
        assert_eq!(note.id, session.to_string());
        assert_eq!(
            note.info,
            keys(&[
                (info::WORKER, worker.as_uuid().as_u128().to_string()),
                (info::SESSION, session.to_string())
            ])
        );
        assert_eq!((note.category, note.urgent, note.thread), (None, true, None));
    }

    /// A small question's options pushed with its request are the note's own buttons: each a
    /// pick carrying its choice, under a category named by the labels, no Allow or Deny; a
    /// press of one answers with that choice where the note is. A body with no request offers
    /// none.
    #[test]
    fn a_questions_options_are_its_notes_buttons() {
        use slopty_proto::thread::wire::NoteChoice;

        use crate::notify::{PICKING, Pressed, Tap, pick_id, picking_id};
        let at = Subject::Thread(ThreadAt { worker: WorkerId::new(), thread: ThreadId::new() });
        let pick = |label: &str| NoteChoice {
            label: label.to_owned(),
            choice: format!("[{{\"question\":\"Layout?\",\"answer\":\"{label}\"}}]"),
        };
        let choices = vec![pick("Split"), pick("Unified")];
        let body = PushBody {
            notice: notice(NoticeKind::NeedsYou, at, None),
            ask: Some(AskId("toolu_02".to_owned())),
            choices: choices.clone(),
            quiet: false,
            merges: None,
        };
        let note = note_of(&body);
        assert_eq!(note.picks, choices);
        assert_eq!(note.category, None, "the picks' own category, no Allow or Deny");
        assert_eq!(note.info.get(&pick_id(1)), Some(&choices[1].choice));
        assert_eq!(note.info.get(info::ASK).map(String::as_str), Some("toolu_02"));
        let id = picking_id(&note.picks);
        assert!(id.starts_with(PICKING), "{id}");
        assert_eq!(id, picking_id(&choices), "the same labels share a category");
        assert_ne!(id, picking_id(&[pick("Split"), pick("Stacked")]));

        let pressed =
            Tap { id: note.id.clone(), info: note.info, action: Some(pick_id(1)), text: None };
        assert!(pressed.finished_later(), "answered where the note is");
        assert_eq!(Pressed::of(&pressed), Some(Pressed::Pick(choices[1].choice.clone())));
        assert_eq!(Pressed::of(&Tap { action: Some(pick_id(3)), ..pressed }), None, "none kept");

        let unasked = note_of(&PushBody { ask: None, ..body });
        assert!(unasked.picks.is_empty(), "no request, nothing to answer");
    }

    /// What the server seals opens on the phone to the note, under its token and key only.
    #[test]
    fn a_push_opens_to_its_note_on_its_phone_only() {
        let key = DeviceKey::generate().unwrap();
        let token = "0f".repeat(32);
        let (worker, session) = (WorkerId::new(), SessionId::new());
        let at = Subject::Thread(ThreadAt { worker, thread: ThreadId::new() });
        let body = PushBody {
            notice: notice(NoticeKind::NeedsYou, at, Some(TermRef { worker, session })),
            ask: None,
            choices: Vec::new(),
            quiet: false,
            merges: None,
        };
        let bytes = slopty_proto::codec::encode_body(&body).unwrap();
        let sealed = slopty_push::seal::seal(&key.public(), &token, &bytes).unwrap();
        let (enc, ct) = (sealed.enc_text(), sealed.ct_text());
        assert_eq!(opened(&enc, &ct, &token, &key).unwrap(), note_of(&body));
        let other = DeviceKey::generate().unwrap();
        assert!(matches!(opened(&enc, &ct, &token, &other), Err(OpenError::Seal(_))));
        assert!(matches!(opened(&enc, &ct, &"1f".repeat(32), &key), Err(OpenError::Seal(_))));
        assert!(matches!(opened("!", &ct, &token, &key), Err(OpenError::NotSealed)));
    }

    /// A take-back names the opaque ids the server pushed notes under, and nothing else: a
    /// note's words, an empty id or more than a push may carry are passed over.
    #[test]
    fn a_take_back_names_only_opaque_ids() {
        let ids = |list: &[&str]| list.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();
        assert_eq!(taken_back(ids(&["a1_b", "Allow Bash?", "", "c-2"])), ids(&["a1_b", "c-2"]));
        let many = vec!["c".to_owned(); slopty_push::apns::MAX_TAKE_BACK + 4];
        assert_eq!(taken_back(many).len(), slopty_push::apns::MAX_TAKE_BACK);
    }
}

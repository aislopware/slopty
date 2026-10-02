//! Each worker's home in Finder: the app's half of the File Provider extension
//! (`apps/slopty-files`, `docs/decisions/platform.md`).
//!
//! The system starts the extension on its own, whether the app is open or not, so the two
//! share a container (the app group [`GROUP`]). The app writes there the workers it knows
//! ([`Directory`]) and keeps one File Provider domain per worker ([`publish`]); the extension
//! reads the directory whenever it opens a worker, and writes back where the system put that
//! worker's root ([`root`]), since asking the system for it would stop the asking process from
//! ever reading the files themselves.
//!
//! A new domain comes up switched off until the person switches it on in System Settings once
//! ([`switch_on`]); [`domains`] tells whether they have.
//!
//! Only a build the team signed has the container ([`container`]): any other one (`cargo run`,
//! a test, an ad hoc bundle) would make the system ask the person whether it may read another
//! app's data, so it touches neither the container nor the domains.

use std::io;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::LazyLock;

use block2::RcBlock;
use objc2::AnyThread as _;
use objc2::rc::Retained;
use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};
use objc2_file_provider::{NSFileProviderDomain, NSFileProviderManager};
use objc2_finder_sync::FIFinderSyncController;
use objc2_foundation::{NSArray, NSError, NSFileManager, NSString, NSURL};
use objc2_security::{
    SecCSFlags, SecCode, SecStaticCode, kSecCSSigningInformation, kSecCodeInfoTeamIdentifier,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use slopty_core::WorkerId;
use tokio::sync::oneshot;

/// The team that signs Slopty, whose identifier prefixes [`GROUP`].
pub const TEAM: &str = "AJ4R8GWM7A";

/// The app group the app and the extension share, prefixed with the signing team, which
/// needs no provisioning profile under a Developer ID.
pub const GROUP: &str = "AJ4R8GWM7A.dev.aislopware.slopty";

/// The directory's file in the shared container.
pub const DIRECTORY: &str = "workers.json";

/// Every worker shown in Finder.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Directory {
    /// The workers, in the order the app lists them.
    pub workers: Vec<Known>,
}

/// One worker, as the extension reaches it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Known {
    /// Its id, which names its domain.
    pub id: WorkerId,
    /// Its name, which names its place in Finder.
    pub name: String,
    /// Where it answers, best first, as `host:port`.
    pub addrs: Vec<String>,
}

impl Directory {
    /// The directory in `dir`; empty when the app has written none yet.
    ///
    /// # Errors
    ///
    /// The file could not be read, or holds no directory.
    pub fn read(dir: &Path) -> io::Result<Self> {
        match std::fs::read(dir.join(DIRECTORY)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Replace the directory in `dir` with this one, whole (`crate::fs::replace`), so a reader
    /// sees one version or the other.
    ///
    /// # Errors
    ///
    /// The file could not be written or moved into place.
    pub fn write(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        crate::fs::replace(&dir.join(DIRECTORY), &serde_json::to_vec_pretty(self)?)
    }

    /// The worker whose domain is `id`.
    #[must_use]
    pub fn get(&self, id: WorkerId) -> Option<&Known> {
        self.workers.iter().find(|w| w.id == id)
    }
}

/// The shared container, which the system makes on first use; `None` for a build the team did
/// not sign, which is not in [`GROUP`].
#[must_use]
pub fn container() -> Option<PathBuf> {
    static SIGNED: LazyLock<bool> = LazyLock::new(|| team().as_deref() == Some(TEAM));
    if !*SIGNED {
        return None;
    }
    let group = NSString::from_str(GROUP);
    let url = NSFileManager::defaultManager()
        .containerURLForSecurityApplicationGroupIdentifier(&group)?;
    url.to_file_path()
}

/// The team that signed this process, from its code signature; `None` for an ad hoc or
/// missing signature.
fn team() -> Option<String> {
    let mut code: *mut SecCode = std::ptr::null_mut();
    // SAFETY: Security rule (SecCode.h): `SecCodeCopySelf` takes no flags and, on success,
    // writes a code object the caller owns into the slot.
    let status = unsafe { SecCode::copy_self(SecCSFlags(0), NonNull::from(&mut code)) };
    // SAFETY: as above: an owned, valid code object once the status is success.
    let code = unsafe { CFRetained::from_raw(NonNull::new(code).filter(|_| status == 0)?) };
    let mut on_disk: *const SecStaticCode = std::ptr::null();
    // SAFETY: Security rule (SecCode.h): `SecCodeCopyStaticCode` takes no flags and, on
    // success, writes a static code object the caller owns into the slot.
    let status = unsafe { code.copy_static_code(SecCSFlags(0), NonNull::from(&mut on_disk)) };
    // SAFETY: as above.
    let on_disk =
        unsafe { CFRetained::from_raw(NonNull::new(on_disk.cast_mut()).filter(|_| status == 0)?) };
    let mut info: *const CFDictionary = std::ptr::null();
    // SAFETY: Security rule (SecCode.h): `SecCodeCopySigningInformation` with
    // `kSecCSSigningInformation` writes a dictionary the caller owns into the slot, which holds
    // `kSecCodeInfoTeamIdentifier` when a team signed the code.
    let status = unsafe {
        SecCode::copy_signing_information(
            &on_disk,
            SecCSFlags(kSecCSSigningInformation),
            NonNull::from(&mut info),
        )
    };
    // SAFETY: as above.
    let info =
        unsafe { CFRetained::from_raw(NonNull::new(info.cast_mut()).filter(|_| status == 0)?) };
    // SAFETY: Security rule (SecCode.h): the signing information's keys are strings and its
    // values CoreFoundation objects.
    let info: &CFDictionary<CFString, CFType> = unsafe { info.cast_unchecked() };
    // SAFETY: Security rule: the key is a constant string.
    let team = info.get(unsafe { kSecCodeInfoTeamIdentifier })?;
    team.downcast::<CFString>().ok().map(|team| team.to_string())
}

/// The file in `dir` that holds where worker `id`'s root is in Finder.
fn root_file(dir: &Path, id: WorkerId) -> PathBuf {
    dir.join("roots").join(id.to_string())
}

/// Where the system put worker `id`'s root, as its extension wrote it; `None` before the
/// extension has run for it.
#[must_use]
pub fn root(dir: &Path, id: WorkerId) -> Option<PathBuf> {
    let path = std::fs::read_to_string(root_file(dir, id)).ok()?;
    let path = PathBuf::from(path);
    path.is_absolute().then_some(path)
}

/// Record that worker `id`'s root is at `at`, for [`root`]: the extension's half.
///
/// # Errors
///
/// The file could not be written.
pub fn set_root(dir: &Path, id: WorkerId, at: &Path) -> io::Result<()> {
    let file = root_file(dir, id);
    if let Some(roots) = file.parent() {
        std::fs::create_dir_all(roots)?;
    }
    crate::fs::replace(&file, at.as_os_str().as_encoded_bytes())
}

/// Forget where worker `id`'s root was, once its domain is removed.
fn forget_root(dir: &Path, id: WorkerId) {
    match std::fs::remove_file(root_file(dir, id)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            tracing::warn!(error = %e, %id, "a removed domain's root left behind");
        }
        _ => {}
    }
}

/// What a File Provider call came back with.
#[derive(Debug, thiserror::Error)]
pub enum FilesError {
    /// The system refused, in these words.
    #[error("{0}")]
    Refused(String),
    /// The build has no shared container: it is not signed into [`GROUP`].
    #[error("this build shares no container with its File Provider extension")]
    NoContainer,
    /// The directory could not be written.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// One worker's domain, as the system has it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Domain {
    /// The worker.
    pub id: WorkerId,
    /// Its name in Finder.
    pub name: String,
    /// The person has switched it on in System Settings.
    pub enabled: bool,
}

/// What [`publish`] asks of the system to make its domains match the workers.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Plan {
    /// The domains of workers no longer shown.
    pub remove: Vec<Domain>,
    /// The workers that have no domain yet, or one under another name.
    pub add: Vec<Known>,
}

/// The plan that takes the domains `have` to one per worker of `want`, each named for it. A
/// domain that already matches is left alone, so its files and its switch stay as they are.
#[must_use]
pub fn plan(have: &[Domain], want: &[Known]) -> Plan {
    Plan {
        remove: have.iter().filter(|d| !want.iter().any(|w| w.id == d.id)).cloned().collect(),
        add: want
            .iter()
            .filter(|w| !have.iter().any(|d| d.id == w.id && d.name == w.name))
            .cloned()
            .collect(),
    }
}

/// Show `workers` in Finder: write them to the shared container, add a domain for each new
/// or renamed one, and remove the domain of each worker no longer among them, its files with
/// it.
///
/// # Errors
///
/// No shared container, the directory could not be written, or the system refused a domain.
pub async fn publish(workers: &[Known]) -> Result<(), FilesError> {
    let dir = container().ok_or(FilesError::NoContainer)?;
    Directory { workers: workers.to_vec() }.write(&dir)?;
    let Plan { remove, add } = plan(&domains().await?, workers);
    for gone in remove {
        done(|handler| {
            // SAFETY: FileProvider rule: `removeDomain:completionHandler:` finds the domain by
            // its identifier and calls the handler exactly once.
            unsafe {
                NSFileProviderManager::removeDomain_completionHandler(
                    &domain(gone.id, &gone.name),
                    handler,
                );
            }
        })
        .await?;
        forget_root(&dir, gone.id);
    }
    for worker in &add {
        // Adding a domain the system already has keeps its files and takes the new name.
        done(|handler| {
            // SAFETY: FileProvider rule: `addDomain:completionHandler:` takes a domain made
            // with an identifier and a display name, and calls the handler exactly once.
            unsafe {
                NSFileProviderManager::addDomain_completionHandler(
                    &domain(worker.id, &worker.name),
                    handler,
                );
            }
        })
        .await?;
    }
    Ok(())
}

/// Every worker's domain the system has, and whether each is switched on.
///
/// # Errors
///
/// The system refused to list them.
pub async fn domains() -> Result<Vec<Domain>, FilesError> {
    listed().await.unwrap_or_else(|_dropped| Err(unanswered()))
}

/// Open System Settings where File Provider extensions are switched on, for the person to
/// switch Slopty's on once. From any thread: it opens from the main one.
pub fn switch_on() {
    dispatch2::DispatchQueue::main().exec_async(|| {
        // SAFETY: FinderSync rule: the class method takes no arguments and only opens System
        // Settings at the extensions, on the main thread, where this block runs.
        unsafe {
            FIFinderSyncController::showExtensionManagementInterface();
        }
    });
}

/// Open the folder `root` (a worker's home, [`root`]) in a Finder window.
pub fn show(root: &Path) {
    let Some(url) = NSURL::from_directory_path(root) else { return };
    let opened = objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
    tracing::debug!(opened, root = %root.display(), "a worker's home in Finder");
}

/// The domain of worker `id`, called `name` in Finder.
fn domain(id: WorkerId, name: &str) -> Retained<NSFileProviderDomain> {
    // SAFETY: FileProvider rule: a domain is made from any identifier and display name.
    unsafe {
        NSFileProviderDomain::initWithIdentifier_displayName(
            NSFileProviderDomain::alloc(),
            &NSString::from_str(&id.to_string()),
            &NSString::from_str(name),
        )
    }
}

/// A domain the system listed, when this app made it.
fn of(domain: &NSFileProviderDomain) -> Option<Domain> {
    // SAFETY: FileProvider rule: `identifier`, `displayName` and `userEnabled` are plain
    // properties of a domain the system listed.
    let id = unsafe { domain.identifier() };
    // SAFETY: as above.
    let name = unsafe { domain.displayName() };
    // SAFETY: as above.
    let enabled = unsafe { domain.userEnabled() };
    Some(Domain { id: id.to_string().parse().ok()?, name: name.to_string(), enabled })
}

fn unanswered() -> FilesError {
    FilesError::Refused("the system did not answer".to_owned())
}

/// Ask the system for its domains; the answer, once its handler runs. A plain function, so
/// no block is held across a wait and the futures above stay `Send`.
fn listed() -> oneshot::Receiver<Result<Vec<Domain>, FilesError>> {
    let (tx, rx) = oneshot::channel();
    let tx = Mutex::new(Some(tx));
    let handler = RcBlock::new(
        move |domains: NonNull<NSArray<NSFileProviderDomain>>, error: *mut NSError| {
            // SAFETY: FileProvider rule: the array is valid for the duration of the handler,
            // and a non-null error is a valid `NSError` for it too.
            let answer = match unsafe { error.as_ref() } {
                Some(error) => Err(FilesError::Refused(error.localizedDescription().to_string())),
                // SAFETY: as above.
                None => Ok(unsafe { domains.as_ref() }.iter().filter_map(|d| of(&d)).collect()),
            };
            let tx = tx.lock().take();
            if let Some(tx) = tx {
                let _unheard = tx.send(answer);
            }
        },
    );
    // SAFETY: FileProvider rule: `getDomainsWithCompletionHandler:` copies the handler and
    // calls it exactly once, on a queue of its choosing; it holds only `Send` data behind a
    // lock.
    unsafe {
        NSFileProviderManager::getDomainsWithCompletionHandler(&handler);
    }
    rx
}

/// Make the call `call` with a completion handler, and wait for the handler's word.
async fn done(
    call: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>),
) -> Result<(), FilesError> {
    asked(call).await.unwrap_or_else(|_dropped| Err(unanswered()))
}

/// Make the call `call` with a completion handler, which the system copies; its word, once
/// it runs. A plain function, as [`listed`] is.
fn asked(
    call: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>),
) -> oneshot::Receiver<Result<(), FilesError>> {
    let (tx, rx) = oneshot::channel();
    let tx = Mutex::new(Some(tx));
    let handler = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: FileProvider rule: a non-null error is a valid `NSError` for the duration of
        // the completion handler.
        let answer = match unsafe { error.as_ref() } {
            Some(error) => Err(FilesError::Refused(error.localizedDescription().to_string())),
            None => Ok(()),
        };
        let tx = tx.lock().take();
        if let Some(tx) = tx {
            let _unheard = tx.send(answer);
        }
    });
    call(&handler);
    rx
}

/// `publish` and `domains` may run on any runtime's thread.
const _: fn() = || {
    const fn send<T: Send>(_: &T) {}
    send(&publish(&[]));
    send(&domains());
};

#[cfg(test)]
mod tests {
    use super::*;

    /// What the app writes the extension reads back whole; before the app writes, there are no
    /// workers, and a file that holds no directory is an error, not an empty one.
    #[test]
    fn the_app_writes_what_the_extension_reads() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Directory::read(dir.path()).unwrap(), Directory::default());
        let studio = Known {
            id: WorkerId::new(),
            name: "mac-studio".to_owned(),
            addrs: vec!["mac-studio.tail1234.ts.net:45550".to_owned()],
        };
        let written = Directory { workers: vec![studio.clone()] };
        written.write(dir.path()).unwrap();
        let read = Directory::read(dir.path()).unwrap();
        assert_eq!(read, written);
        assert_eq!(read.get(studio.id), Some(&studio));
        assert_eq!(read.get(WorkerId::new()), None);
        std::fs::write(dir.path().join(DIRECTORY), b"not json").unwrap();
        Directory::read(dir.path()).unwrap_err();
    }

    /// A root the extension writes is the root the app reads, per worker; none before it is
    /// written, and a path that is not absolute is none.
    #[test]
    fn the_extension_tells_the_app_where_a_root_is() {
        let dir = tempfile::tempdir().unwrap();
        let (studio, pro) = (WorkerId::new(), WorkerId::new());
        assert_eq!(root(dir.path(), studio), None);
        let at = Path::new("/Users/dev/Library/CloudStorage/Slopty-mac-studio");
        set_root(dir.path(), studio, at).unwrap();
        assert_eq!(root(dir.path(), studio).as_deref(), Some(at));
        assert_eq!(root(dir.path(), pro), None);
        set_root(dir.path(), pro, Path::new("relative")).unwrap();
        assert_eq!(root(dir.path(), pro), None);
        forget_root(dir.path(), studio);
        assert_eq!(root(dir.path(), studio), None, "gone with its domain");
        forget_root(dir.path(), studio);
    }

    fn known(name: &str) -> Known {
        Known { id: WorkerId::new(), name: name.to_owned(), addrs: vec![format!("{name}:45550")] }
    }

    fn domain_of(worker: &Known, enabled: bool) -> Domain {
        Domain { id: worker.id, name: worker.name.clone(), enabled }
    }

    /// Each worker gets a domain once: a new worker is added, a renamed one added again under
    /// its new name, one that matches left alone whether or not it is switched on, and the
    /// domain of a worker no longer shown removed.
    #[test]
    fn one_domain_per_worker_and_none_for_a_forgotten_one() {
        let (studio, pro, air) = (known("mac-studio"), known("macbook-pro"), known("air"));
        assert_eq!(
            plan(&[], &[studio.clone(), pro.clone()]),
            Plan { remove: vec![], add: vec![studio.clone(), pro.clone()] },
            "the first time"
        );
        let have = [domain_of(&studio, true), domain_of(&pro, false), domain_of(&air, true)];
        let renamed = Known { name: "studio".to_owned(), ..studio.clone() };
        assert_eq!(
            plan(&have, &[renamed.clone(), pro.clone()]),
            Plan { remove: vec![domain_of(&air, true)], add: vec![renamed] },
            "the forgotten worker goes, the renamed one is named again"
        );
        assert_eq!(plan(&have, &[studio, pro, air]), Plan::default(), "nothing to do");
        assert_eq!(
            plan(&have[..1], &[]),
            Plan { remove: have[..1].to_vec(), add: vec![] },
            "no workers, no domains"
        );
    }

    /// The group is the team's, and a build the team did not sign, this test's own, has no
    /// container: it never touches another app's data, which would ask the person.
    #[test]
    fn only_a_build_the_team_signed_has_the_container() {
        assert!(GROUP.starts_with(&format!("{TEAM}.")));
        assert_eq!(team(), None, "the test binary is signed ad hoc");
        assert_eq!(container(), None);
    }
}

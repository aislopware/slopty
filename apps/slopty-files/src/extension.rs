//! The face the system calls: the replicated File Provider extension, its items and its
//! enumerators, each a thin Objective-C class over a [`Domain`].
//!
//! The system calls in on queues of its own and hands completion handlers that may be called
//! from any thread, so each call copies its handler, starts the work on the extension's own
//! runtime and returns at once. What Finder makes, renames, moves, saves or trashes in the
//! domain is done on the worker, and nothing there is lost or unlinked: a new file lands
//! beside the others, a file saved in place replaces the worker's only while that is the
//! version it was opened at (else the save lands beside it as a conflicted copy), and a
//! trashed one goes to the worker's own trash.

use std::ffi::{CString, c_char, c_int};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{
    AnyThread as _, ClassType as _, DefinedClass as _, Message as _, define_class, msg_send, sel,
};
use objc2_file_provider::{
    NSFileProviderChangeObserver, NSFileProviderCreateItemOptions, NSFileProviderDeleteItemOptions,
    NSFileProviderDomain, NSFileProviderEnumerating, NSFileProviderEnumerationObserver,
    NSFileProviderEnumerator, NSFileProviderErrorCode, NSFileProviderErrorDomain,
    NSFileProviderFileSystemFlags, NSFileProviderItem, NSFileProviderItemCapabilities,
    NSFileProviderItemFields, NSFileProviderItemProtocol, NSFileProviderItemVersion,
    NSFileProviderManager, NSFileProviderModifyItemOptions, NSFileProviderReplicatedExtension,
    NSFileProviderRequest, NSFileProviderRootContainerItemIdentifier,
    NSFileProviderTrashContainerItemIdentifier, NSFileProviderWorkingSetContainerItemIdentifier,
};
use objc2_foundation::{
    NSArray, NSCocoaErrorDomain, NSData, NSDate, NSError, NSNumber, NSProgress, NSString, NSURL,
    NSUserCancelledError,
};
use objc2_uniform_type_identifiers::{
    UTType, UTTypeAliasFile, UTTypeData, UTTypeFolder, UTTypePackage, UTTypeSymbolicLink,
};
use slopty_client::xfer::{Brought, XferError};
use slopty_core::{WorkerId, XferId};
use tokio::runtime::Runtime;

use crate::changes::Change;
use crate::domain::{Domain, Page, Signal, Written};
use crate::item::{self, Item};
use crate::pages::Cursor;
use crate::worker::FilesError;

unsafe extern "C" {
    /// Foundation's entry point for an app extension: it connects to the system and serves
    /// the principal class the bundle's `NSExtension` names, and never returns.
    fn NSExtensionMain(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

/// The extension's runtime, on which every call's work runs; `None` when it could not start.
static RUNTIME: LazyLock<Option<Runtime>> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("slopty-files")
        .enable_all()
        .build()
        .inspect_err(|e| tracing::error!(error = %e, "the extension's runtime"))
        .ok()
});

/// Run the extension: register its classes and hand the process to the system.
#[must_use]
pub fn main() -> c_int {
    log_to_container();
    let _registered = (Extension::class(), FileItem::class(), Enumerator::class());
    let args: Vec<CString> =
        std::env::args_os().filter_map(|arg| CString::new(arg.into_encoded_bytes()).ok()).collect();
    let mut argv: Vec<*mut c_char> = args
        .iter()
        .map(|arg| arg.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect();
    let argc = c_int::try_from(args.len()).unwrap_or(c_int::MAX);
    // SAFETY: Foundation's rule for `NSExtensionMain`: called once, from `main`, with the
    // process's arguments, a null-terminated array whose strings outlive the call, which
    // never returns while the extension runs.
    unsafe { NSExtensionMain(argc, argv.as_mut_ptr()) }
}

/// The extension's log, in the container it shares with the app, where its stderr is not.
fn log_to_container() {
    let Some(file) = slopty_platform::files::container()
        .and_then(|dir| std::fs::File::create(dir.join("extension.log")).ok())
    else {
        return;
    };
    let _set = tracing_subscriber::fmt().with_writer(Arc::new(file)).with_ansi(false).try_init();
}

/// Run `work` on the extension's runtime; `false` when there is none.
fn spawn(work: impl Future<Output = ()> + Send + 'static) -> bool {
    RUNTIME.as_ref().map(|runtime| runtime.spawn(work)).is_some()
}

/// A copied completion handler, called once from whichever thread the work ends on.
struct Reply<F: ?Sized>(RcBlock<F>);

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the block is only called, which FileProvider allows from any thread"
)]
// SAFETY: FileProvider rule: a completion handler may be called from any thread; the block is
// a heap copy, immutable once made, and called once.
unsafe impl<F: ?Sized> Send for Reply<F> {}

/// A system object the work holds on to and calls on another thread.
struct Held<T: ?Sized>(Retained<T>);

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "FileProvider's observers and managers are called from any thread"
)]
// SAFETY: FileProvider rule: an enumeration observer, a change observer and a manager may be
// called from any thread, and the system expects them called from the extension's own queues.
unsafe impl<T: ?Sized> Send for Held<T> {}
// SAFETY: as above.
unsafe impl<T: ?Sized> Sync for Held<T> {}

impl<T: ?Sized> Held<T> {
    /// The object; a closure that calls it this way holds the whole `Held`, which is `Send`.
    fn get(&self) -> &T {
        &self.0
    }
}

/// The identifier the system knows `id` by.
fn identifier(id: &str) -> Retained<NSString> {
    if id == item::ROOT {
        // SAFETY: FileProvider rule: the root's identifier is a constant string.
        unsafe { NSFileProviderRootContainerItemIdentifier }.retain()
    } else {
        NSString::from_str(id)
    }
}

/// The item the system names `identifier`.
fn id_of(identifier: &NSString) -> String {
    // SAFETY: FileProvider rule: the root's identifier is a constant string.
    if identifier.isEqualToString(unsafe { NSFileProviderRootContainerItemIdentifier }) {
        item::ROOT.to_owned()
    } else {
        identifier.to_string()
    }
}

/// The error the system is told for `error`.
fn ns_error(error: &FilesError) -> Retained<NSError> {
    let provider = provider_error;
    match error {
        FilesError::NoSuchItem(_) | FilesError::NotFolder(_) | FilesError::Refused { .. } => {
            provider(NSFileProviderErrorCode::NoSuchItem)
        }
        FilesError::Unreachable(_)
        | FilesError::WrongWorker { .. }
        | FilesError::Transfer(
            XferError::LinkClosed | XferError::Cut(_) | XferError::Unanswered(_),
        ) => provider(NSFileProviderErrorCode::ServerUnreachable),
        FilesError::Transfer(XferError::Cancelled) => cocoa(NSUserCancelledError),
        FilesError::Clash(_) => provider(NSFileProviderErrorCode::FilenameCollision),
        FilesError::Transfer(_)
        | FilesError::Declined { .. }
        | FilesError::Changed { .. }
        | FilesError::Failed { .. } => provider(NSFileProviderErrorCode::CannotSynchronize),
    }
}

/// The File Provider error `code`.
fn provider_error(code: NSFileProviderErrorCode) -> Retained<NSError> {
    // SAFETY: FileProvider rule: the domain is a constant string, and a code of its own with no
    // user info makes a valid error.
    unsafe { NSError::errorWithDomain_code_userInfo(NSFileProviderErrorDomain, code.0, None) }
}

/// Foundation's error `code`.
fn cocoa(code: isize) -> Retained<NSError> {
    // SAFETY: Foundation rule: its domain is a constant string, and its own code with no user
    // info makes a valid error.
    unsafe { NSError::errorWithDomain_code_userInfo(NSCocoaErrorDomain, code, None) }
}

/// A progress already at its end, for a call answered at once.
fn finished() -> Retained<NSProgress> {
    let progress = NSProgress::discreteProgressWithTotalUnitCount(1);
    progress.setCompletedUnitCount(1);
    progress
}

/// The items `items` as the system takes a list of them.
fn items_array(items: Vec<Item>) -> Retained<NSArray<NSFileProviderItem>> {
    let items: Vec<Retained<NSFileProviderItem>> =
        items.into_iter().map(|item| ProtocolObject::from_retained(FileItem::new(item))).collect();
    NSArray::from_retained_slice(&items)
}

/// One worker's extension: what the system knows of its domain, and the domain itself.
struct Ivars {
    domain: Arc<Domain>,
    system: Retained<NSFileProviderDomain>,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Extension` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "SloptyFilesExtension"]
    #[ivars = Option<Ivars>]
    struct Extension;

    impl Extension {}

    unsafe impl NSObjectProtocol for Extension {}

    unsafe impl NSFileProviderReplicatedExtension for Extension {
        /// The system starts the extension for one of its domains.
        #[unsafe(method_id(initWithDomain:))]
        fn init_with_domain(
            this: Allocated<Self>,
            domain: &NSFileProviderDomain,
        ) -> Option<Retained<Self>> {
            let ivars = start(domain);
            let this = this.set_ivars(ivars);
            // SAFETY: `NSObject`'s `init` takes nothing and returns the same object.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(invalidate))]
        fn closed(&self) {
            tracing::info!("the system closed the domain");
        }

        #[unsafe(method_id(itemForIdentifier:request:completionHandler:))]
        fn item_for_identifier(
            &self,
            identifier: &NSString,
            _request: &NSFileProviderRequest,
            completion: &DynBlock<dyn Fn(*mut NSFileProviderItem, *mut NSError)>,
        ) -> Retained<NSProgress> {
            let reply = Reply(completion.copy());
            let progress = NSProgress::discreteProgressWithTotalUnitCount(1);
            let done = Retained::clone(&progress);
            let (id, domain) = (id_of(identifier), self.domain());
            let started = spawn(async move {
                let found = match domain {
                    Some(domain) => domain.item(&id).await,
                    None => Err(FilesError::Unreachable("no domain".to_owned())),
                };
                answer_item(&reply, found);
                done.setCompletedUnitCount(1);
            });
            if !started {
                tracing::error!("no runtime to look an item up on");
            }
            progress
        }

        #[unsafe(method_id(fetchContentsForItemWithIdentifier:version:request:completionHandler:))]
        fn fetch_contents(
            &self,
            identifier: &NSString,
            _version: Option<&NSFileProviderItemVersion>,
            _request: &NSFileProviderRequest,
            completion: &DynBlock<dyn Fn(*mut NSURL, *mut NSFileProviderItem, *mut NSError)>,
        ) -> Retained<NSProgress> {
            self.fetch(identifier, completion)
        }

        #[unsafe(method_id(createItemBasedOnTemplate:fields:contents:options:request:completionHandler:))]
        fn create_item(
            &self,
            template: &NSFileProviderItem,
            _fields: NSFileProviderItemFields,
            contents: Option<&NSURL>,
            options: NSFileProviderCreateItemOptions,
            _request: &NSFileProviderRequest,
            completion: &DynBlock<
                dyn Fn(*mut NSFileProviderItem, NSFileProviderItemFields, Bool, *mut NSError),
            >,
        ) -> Retained<NSProgress> {
            self.create(template, contents, options, completion)
        }

        #[unsafe(method_id(modifyItem:baseVersion:changedFields:contents:options:request:completionHandler:))]
        fn modify_item(
            &self,
            item: &NSFileProviderItem,
            version: &NSFileProviderItemVersion,
            fields: NSFileProviderItemFields,
            contents: Option<&NSURL>,
            _options: NSFileProviderModifyItemOptions,
            _request: &NSFileProviderRequest,
            completion: &DynBlock<
                dyn Fn(*mut NSFileProviderItem, NSFileProviderItemFields, Bool, *mut NSError),
            >,
        ) -> Retained<NSProgress> {
            self.modify(item, (version, fields, contents), completion)
        }

        #[unsafe(method_id(deleteItemWithIdentifier:baseVersion:options:request:completionHandler:))]
        fn delete_item(
            &self,
            identifier: &NSString,
            _version: &NSFileProviderItemVersion,
            options: NSFileProviderDeleteItemOptions,
            _request: &NSFileProviderRequest,
            completion: &DynBlock<dyn Fn(*mut NSError)>,
        ) -> Retained<NSProgress> {
            self.delete(identifier, options, completion)
        }
    }

    unsafe impl NSFileProviderEnumerating for Extension {
        #[unsafe(method_id(enumeratorForContainerItemIdentifier:request:error:))]
        fn enumerator_for(
            &self,
            container: &NSString,
            _request: &NSFileProviderRequest,
            error: *mut *mut NSError,
        ) -> Option<Retained<ProtocolObject<dyn NSFileProviderEnumerator>>> {
            self.enumerator(container, error)
        }
    }
);

impl Extension {
    /// Fetch the file `identifier` into the domain's temporary directory, as
    /// `fetchContentsForItemWithIdentifier:version:request:completionHandler:` asks; a cancel
    /// of the progress returned stops the transfer.
    fn fetch(
        &self,
        identifier: &NSString,
        completion: &DynBlock<dyn Fn(*mut NSURL, *mut NSFileProviderItem, *mut NSError)>,
    ) -> Retained<NSProgress> {
        let reply = Reply(completion.copy());
        let (Some(domain), Some(temporary)) = (self.domain(), self.temporary()) else {
            let error = ns_error(&no_domain());
            reply.0.call((
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                Retained::as_ptr(&error).cast_mut(),
            ));
            return finished();
        };
        let xfer = XferId::new();
        let progress = cancellable(&domain, xfer);
        let done = Retained::clone(&progress);
        let id = id_of(identifier);
        let started = spawn(async move {
            let into = temporary.join(xfer.to_string());
            let (seen, mut heard) = tokio::sync::watch::channel(Brought::default());
            let fed = Retained::clone(&done);
            // Ends when the fetch does, which drops the sender.
            let _feeding = spawn(async move {
                while heard.changed().await.is_ok() {
                    let (total, landed) = units(&heard.borrow_and_update());
                    fed.setTotalUnitCount(total);
                    fed.setCompletedUnitCount(landed);
                }
            });
            let fetched = match tokio::fs::create_dir_all(&into).await {
                Ok(()) => domain.fetch(&id, &into, xfer, Some(seen)).await,
                Err(e) => Err(FilesError::Transfer(XferError::Local {
                    path: into.display().to_string(),
                    source: e,
                })),
            };
            answer_contents(&reply, fetched);
            done.setCompletedUnitCount(done.totalUnitCount());
        });
        if !started {
            tracing::error!("no runtime to fetch a file on");
        }
        progress
    }

    /// Make on the worker what was made in Finder, as
    /// `createItemBasedOnTemplate:fields:contents:options:request:completionHandler:` asks: a
    /// folder with `MakeDir`, a file sent up beside the others in its folder, where nothing is
    /// written over (a taken name lands as the next free one, which the system then shows). An
    /// item made again after the domain was reset (`MayAlreadyExist`) is the one already there
    /// when there is one, so nothing is sent twice. A link, an alias or a package stays on this
    /// Mac only. A cancel of the progress returned stops the upload.
    fn create(
        &self,
        template: &NSFileProviderItem,
        contents: Option<&NSURL>,
        options: NSFileProviderCreateItemOptions,
        completion: &ChangeHandler,
    ) -> Retained<NSProgress> {
        let reply = Reply(completion.copy());
        let (Some(domain), Some(temporary)) = (self.domain(), self.temporary()) else {
            answer_change(&reply, Err(Unchanged::Error(no_domain())), Fields::empty());
            return finished();
        };
        let (_made_as, parent, name) = placed(template);
        let parent = id_of(&parent);
        let kind = made_as(template);
        if kind == Made::Kept {
            tracing::info!(%parent, %name, "a link, an alias or a package kept on this Mac");
            let kept = provider_error(NSFileProviderErrorCode::ExcludedFromSync);
            answer_change(&reply, Err(Unchanged::Refused(kept)), Fields::empty());
            return finished();
        }
        let local = contents.and_then(NSURL::to_file_path);
        let again = options.contains(NSFileProviderCreateItemOptions::MayAlreadyExist);
        let xfer = XferId::new();
        let progress = cancellable(&domain, xfer);
        let done = Retained::clone(&progress);
        let started = spawn(async move {
            let to = (parent.as_str(), name.as_str());
            let made = make(&domain, to, kind, local, again, (&temporary, xfer)).await;
            answer_change(&reply, made, Fields::empty());
            done.setCompletedUnitCount(1);
        });
        if !started {
            tracing::error!("no runtime to make an item on");
        }
        progress
    }

    /// Do on the worker what was done to an item in Finder, as
    /// `modifyItem:baseVersion:changedFields:contents:options:request:completionHandler:` asks:
    /// a rename or a move with `Move`, a move to the trash with `Trash`, to the worker's own
    /// trash, where the person can put it back, and a file saved in place with `Replace`, over
    /// the version it was opened at ([`Domain::replace`]). A change the worker refuses is
    /// undone here, the item put back as the worker has it; a file saved over a change made
    /// on the worker meanwhile keeps the worker's, which the system fetches again, and what
    /// was saved lands beside it as a conflicted copy. The rest (its dates, its tags) stays
    /// here. A cancel of the progress returned stops a save's upload.
    fn modify(
        &self,
        item: &NSFileProviderItem,
        (version, fields, contents): (&NSFileProviderItemVersion, Fields, Option<&NSURL>),
        completion: &ChangeHandler,
    ) -> Retained<NSProgress> {
        let reply = Reply(completion.copy());
        let moved = Fields::Filename | Fields::ParentItemIdentifier;
        let local =
            contents.filter(|_| fields.contains(Fields::Contents)).and_then(NSURL::to_file_path);
        let saving = local.is_some();
        let written = if saving { Fields::Contents } else { Fields::empty() };
        let pending = fields.difference(moved | written);
        let (Some(domain), Some(temporary)) = (self.domain(), self.temporary()) else {
            answer_change(&reply, Err(Unchanged::Error(no_domain())), pending);
            return finished();
        };
        // SAFETY: FileProvider rule: a version's content version is a plain property, the
        // data this extension gave the item it was made from.
        let base = item::version_of(&unsafe { version.contentVersion() }.to_vec());
        let (id, parent, name) = placed(item);
        // SAFETY: FileProvider rule: the trash's identifier is a constant string.
        let trashed = parent.isEqualToString(unsafe { NSFileProviderTrashContainerItemIdentifier });
        let (id, parent) = (id_of(&id), id_of(&parent));
        let xfer = XferId::new();
        let progress = cancellable(&domain, xfer);
        let done = Retained::clone(&progress);
        let started = spawn(async move {
            let changed = if trashed {
                domain.trash(&id).await.map(|_trashed| Changed::Left(None))
            } else {
                let save = local.as_deref().map(|local| (local, base, (temporary.as_path(), xfer)));
                changed(&domain, (&id, &parent, &name), fields.intersects(moved), save).await
            };
            let answer = match changed {
                Ok(Changed::Left(item)) => Ok((item, false)),
                Ok(Changed::Kept(item)) => Ok((Some(item), true)),
                Err(FilesError::NoSuchItem(gone)) => {
                    tracing::info!(%gone, "changed in Finder, gone on the worker");
                    Ok((None, false))
                }
                Err(e @ (FilesError::Declined { .. } | FilesError::Failed { .. })) => {
                    tracing::info!(%id, error = %e, "a change undone");
                    // A save undone takes the worker's contents back too.
                    domain.item(&id).await.map(|i| (Some(i), saving)).map_err(Unchanged::Error)
                }
                Err(e) => Err(Unchanged::Error(e)),
            };
            answer_fetched(&reply, answer, pending);
            done.setCompletedUnitCount(1);
        });
        if !started {
            tracing::error!("no runtime to change an item on");
        }
        progress
    }

    /// Take away on the worker what was deleted under the domain outside Finder (Finder itself
    /// only moves to the trash), as
    /// `deleteItemWithIdentifier:baseVersion:options:request:completionHandler:` asks: to the
    /// worker's own trash, never unlinked. A folder still holding items there is not taken
    /// unless the deletion is recursive; one already gone is.
    fn delete(
        &self,
        identifier: &NSString,
        options: NSFileProviderDeleteItemOptions,
        completion: &DynBlock<dyn Fn(*mut NSError)>,
    ) -> Retained<NSProgress> {
        let reply = Reply(completion.copy());
        let Some(domain) = self.domain() else {
            let error = ns_error(&no_domain());
            reply.0.call((Retained::as_ptr(&error).cast_mut(),));
            return finished();
        };
        let id = id_of(identifier);
        let recursive = options.contains(NSFileProviderDeleteItemOptions::Recursive);
        let progress = NSProgress::discreteProgressWithTotalUnitCount(1);
        let done = Retained::clone(&progress);
        let started = spawn(async move {
            let held = match domain.item(&id).await {
                Ok(item) => item.folder && item.children.is_some_and(|n| n > 0),
                Err(_) => false,
            };
            let error = if held && !recursive {
                Some(provider_error(NSFileProviderErrorCode::DirectoryNotEmpty))
            } else {
                domain.trash(&id).await.err().map(|e| ns_error(&e))
            };
            answer_deleted(&reply, error.as_deref());
            done.setCompletedUnitCount(1);
        });
        if !started {
            tracing::error!("no runtime to delete an item on");
        }
        progress
    }

    /// The enumerator of `container`, as `enumeratorForContainerItemIdentifier:request:error:`
    /// asks; `None`, its error in `error`, when the extension has no domain.
    fn enumerator(
        &self,
        container: &NSString,
        error: *mut *mut NSError,
    ) -> Option<Retained<ProtocolObject<dyn NSFileProviderEnumerator>>> {
        // SAFETY: FileProvider rule: the working set's identifier is a constant string.
        let working = unsafe { NSFileProviderWorkingSetContainerItemIdentifier };
        let folder = (!container.isEqualToString(working)).then(|| id_of(container));
        let Some(domain) = self.domain() else {
            let refused = ns_error(&FilesError::Unreachable("no domain".to_owned()));
            // SAFETY: Objective-C's error-out rule: a non-null `error` points at a slot
            // the caller owns, which takes an autoreleased error.
            if let Some(slot) = unsafe { error.as_mut() } {
                *slot = Retained::autorelease_ptr(refused);
            }
            return None;
        };
        let enumerator = Enumerator::alloc().set_ivars(Listing { domain, folder });
        // SAFETY: `NSObject`'s `init` takes nothing and returns the same object.
        let enumerator: Retained<Enumerator> = unsafe { msg_send![super(enumerator), init] };
        Some(ProtocolObject::from_retained(enumerator))
    }

    fn domain(&self) -> Option<Arc<Domain>> {
        self.ivars().as_ref().map(|ivars| Arc::clone(&ivars.domain))
    }

    /// Where the system takes a fetched file from: its temporary directory for the domain,
    /// on the volume the domain is on.
    fn temporary(&self) -> Option<PathBuf> {
        let ivars = self.ivars().as_ref()?;
        // SAFETY: FileProvider rule: a manager is made for any domain the system started the
        // extension with.
        let manager = unsafe { NSFileProviderManager::managerForDomain(&ivars.system) }?;
        // SAFETY: FileProvider rule: the manager's temporary directory may be asked from any
        // thread.
        let url = unsafe { manager.temporaryDirectoryURLWithError() }
            .inspect_err(
                |e| tracing::warn!(error = %e.localizedDescription(), "no temporary directory"),
            )
            .ok()?;
        url.to_file_path()
    }
}

/// What the extension holds for `system`'s domain: the worker its identifier names, reached
/// at the addresses the app wrote. Also writes where the system put the domain's root, for
/// the app to name worker files by.
fn start(system: &NSFileProviderDomain) -> Option<Ivars> {
    // SAFETY: FileProvider rule: `identifier` is a plain property of the domain.
    let named = unsafe { system.identifier() }.to_string();
    let Ok(id) = named.parse::<WorkerId>() else {
        tracing::error!(domain = %named, "a domain no worker's id names");
        return None;
    };
    let Some(shared) = slopty_platform::files::container() else {
        tracing::error!("the extension shares no container with the app");
        return None;
    };
    // SAFETY: FileProvider rule: a manager is made for any domain the system started the
    // extension with.
    let manager = unsafe { NSFileProviderManager::managerForDomain(system) }.map(Held);
    let Some(manager) = manager.map(Arc::new) else {
        tracing::error!(%id, "no manager for the domain");
        return None;
    };
    record_root(&manager, &shared, id);
    let signal: Signal = Arc::new(move || signal_working_set(&manager));
    Some(Ivars { domain: Arc::new(Domain::new(id, shared, signal)), system: system.retain() })
}

/// Tell the system there are changes in the working set to ask for.
fn signal_working_set(manager: &Held<NSFileProviderManager>) {
    let handler = RcBlock::new(|error: *mut NSError| {
        // SAFETY: FileProvider rule: a non-null error is a valid `NSError` for the duration
        // of the completion handler.
        if let Some(error) = unsafe { error.as_ref() } {
            tracing::warn!(error = %error.localizedDescription(), "the working set's signal");
        }
    });
    // SAFETY: FileProvider rule: a replicated extension signals the working set only, from
    // any thread, and the handler is copied and called once.
    unsafe {
        manager.get().signalEnumeratorForContainerItemIdentifier_completionHandler(
            NSFileProviderWorkingSetContainerItemIdentifier,
            &handler,
        );
    }
}

/// Ask where the system put the domain's root and write it down for the app. The extension
/// may ask, since it never reads its own domain's files; the app may not, since asking would
/// stop it from reading them ever after.
fn record_root(manager: &Held<NSFileProviderManager>, shared: &Path, id: WorkerId) {
    let shared = shared.to_path_buf();
    let handler = RcBlock::new(move |url: *mut NSURL, error: *mut NSError| {
        // SAFETY: FileProvider rule: the URL and the error are each null or valid for the
        // duration of the completion handler.
        let url = unsafe { url.as_ref() };
        // SAFETY: as above.
        let error = unsafe { error.as_ref() };
        match (url.and_then(NSURL::to_file_path), error) {
            (Some(root), _) => {
                if let Err(e) = slopty_platform::files::set_root(&shared, id, &root) {
                    tracing::warn!(error = %e, "the domain's root not written down");
                }
            }
            (None, error) => tracing::warn!(
                error = ?error.map(|e| e.localizedDescription().to_string()),
                "where the domain's root is"
            ),
        }
    });
    // SAFETY: FileProvider rule: the root's identifier is a constant string, and the handler
    // is copied and called once, from any thread.
    unsafe {
        manager.get().getUserVisibleURLForItemIdentifier_completionHandler(
            NSFileProviderRootContainerItemIdentifier,
            &handler,
        );
    }
}

/// Answer an item lookup.
fn answer_item(
    reply: &Reply<dyn Fn(*mut NSFileProviderItem, *mut NSError)>,
    found: Result<Item, FilesError>,
) {
    match found {
        Ok(item) => {
            let item: Retained<NSFileProviderItem> =
                ProtocolObject::from_retained(FileItem::new(item));
            reply.0.call((Retained::as_ptr(&item).cast_mut(), std::ptr::null_mut()));
        }
        Err(e) => {
            tracing::info!(error = %e, "an item not found");
            let error = ns_error(&e);
            reply.0.call((std::ptr::null_mut(), Retained::as_ptr(&error).cast_mut()));
        }
    }
}

/// Answer a fetch with the file that landed, or why none did.
fn answer_contents(
    reply: &Reply<dyn Fn(*mut NSURL, *mut NSFileProviderItem, *mut NSError)>,
    fetched: Result<(PathBuf, Item), FilesError>,
) {
    match fetched {
        Ok((landed, item)) => {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&landed.to_string_lossy()));
            let item: Retained<NSFileProviderItem> =
                ProtocolObject::from_retained(FileItem::new(item));
            reply.0.call((
                Retained::as_ptr(&url).cast_mut(),
                Retained::as_ptr(&item).cast_mut(),
                std::ptr::null_mut(),
            ));
        }
        Err(e) => {
            tracing::info!(error = %e, "a file not fetched");
            let error = ns_error(&e);
            reply.0.call((
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                Retained::as_ptr(&error).cast_mut(),
            ));
        }
    }
}

/// The fields of an item a change names.
type Fields = NSFileProviderItemFields;

/// What a creation or a change is answered with: the item it left, the fields still to apply,
/// whether to fetch its contents again, and why it failed.
type ChangeFn = dyn Fn(*mut NSFileProviderItem, Fields, Bool, *mut NSError);

/// The completion handler of a creation or a change.
type ChangeHandler = DynBlock<ChangeFn>;

/// Why a creation or a change left nothing on the worker.
enum Unchanged {
    /// The worker could not be reached, or would not or could not make it.
    Error(FilesError),
    /// The extension keeps it from the worker, with this error.
    Refused(Retained<NSError>),
}

/// What a template makes on the worker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Made {
    Folder,
    File,
    /// Nothing: a link, an alias or a package, which stays on this Mac.
    Kept,
}

/// Where `item` is: its identifier, its folder's and its name.
fn placed(item: &NSFileProviderItem) -> (Retained<NSString>, Retained<NSString>, String) {
    // SAFETY: FileProvider rule: an item's identifier is a plain property, read on the call's
    // own thread.
    let id = unsafe { item.itemIdentifier() };
    // SAFETY: as above, its folder's.
    let parent = unsafe { item.parentItemIdentifier() };
    // SAFETY: as above, its name.
    let name = unsafe { item.filename() };
    (id, parent, name.to_string())
}

/// What `template` makes, by its type: a file when it names none.
fn made_as(template: &NSFileProviderItem) -> Made {
    if !template.respondsToSelector(sel!(contentType)) {
        return Made::File;
    }
    // SAFETY: FileProvider rule: `contentType` is optional, and the template answers it.
    let kind = unsafe { template.contentType() };
    // SAFETY: UniformTypeIdentifiers rule: the core types are constant objects.
    let (folder, link, alias, package) =
        unsafe { (UTTypeFolder, UTTypeSymbolicLink, UTTypeAliasFile, UTTypePackage) };
    if [link, alias, package].iter().any(|kept| kind.conformsToType(kept)) {
        Made::Kept
    } else if kind.conformsToType(folder) {
        Made::Folder
    } else {
        Made::File
    }
}

/// What a change in Finder left on the worker.
enum Changed {
    /// The item as it is there now, or none when it is gone.
    Left(Option<Item>),
    /// The file as the worker kept it, its save landed beside it: fetched again.
    Kept(Item),
}

/// Do on the worker what was done to the item `id`, now `name` in `parent`: moved there when
/// it `moved`, then its contents saved over when `save` names the file they are in, the
/// version they were made from, and where to stage a copy.
async fn changed(
    domain: &Domain,
    (id, parent, name): (&str, &str, &str),
    moved: bool,
    save: Option<(&Path, Option<slopty_proto::folder::FileVersion>, (&Path, XferId))>,
) -> Result<Changed, FilesError> {
    let at = if moved { domain.rename(id, parent, name).await? } else { domain.item(id).await? };
    let Some((local, base, staging)) = save else {
        return Ok(Changed::Left(Some(at)));
    };
    Ok(match domain.replace(&at.id, local, base, staging).await? {
        Written::Replaced(item) => Changed::Left(Some(item)),
        Written::Kept { now, copy } => {
            tracing::info!(id = %now.id, copy = %copy.id, "a save kept beside a change on the worker");
            Changed::Kept(now)
        }
    })
}

/// Make the item `name` of the folder `parent` on the worker, a folder or a file from the
/// contents at `local` (an empty one when none). Made `again`, it is the item already there
/// when there is one, and nothing when it is a file with no contents to send that is not.
async fn make(
    domain: &Domain,
    (parent, name): (&str, &str),
    kind: Made,
    local: Option<PathBuf>,
    again: bool,
    (temporary, xfer): (&Path, XferId),
) -> Result<Option<Item>, Unchanged> {
    let Some(id) = item::child(parent, name) else {
        let why = format!("“{name}” cannot be a file’s name");
        return Err(Unchanged::Error(FilesError::Declined { path: name.to_owned(), why }));
    };
    if again {
        match domain.item(&id).await {
            Ok(there) => return Ok(Some(there)),
            Err(FilesError::NoSuchItem(_) | FilesError::Refused { .. }) => {}
            Err(e) => return Err(Unchanged::Error(e)),
        }
        if kind == Made::File && local.is_none() {
            return Ok(None);
        }
    }
    let made = match kind {
        Made::Folder => domain.make_folder(parent, name).await,
        Made::File | Made::Kept => {
            send_up(domain, (parent, name), local.as_deref(), temporary, xfer).await
        }
    };
    made.map(Some).map_err(Unchanged::Error)
}

/// Send the contents at `local` (an empty file when none) up into the folder `parent` as
/// `name`: put under that name in a directory of its own in `temporary`, since the system's
/// file is named otherwise, and the upload names a file by its own name.
async fn send_up(
    domain: &Domain,
    (parent, name): (&str, &str),
    local: Option<&Path>,
    temporary: &Path,
    xfer: XferId,
) -> Result<Item, FilesError> {
    let staging = temporary.join(xfer.to_string());
    let staged = staging.join(name);
    let put = async {
        tokio::fs::create_dir_all(&staging).await?;
        match local {
            // A link where the volume allows it, else a copy (a clone on APFS).
            Some(from) => {
                if tokio::fs::hard_link(from, &staged).await.is_err() {
                    tokio::fs::copy(from, &staged).await?;
                }
            }
            None => drop(tokio::fs::File::create(&staged).await?),
        }
        Ok::<(), std::io::Error>(())
    };
    let sent = match put.await {
        Ok(()) => domain.create_file(parent, &staged, xfer).await,
        Err(source) => Err(FilesError::Transfer(XferError::Local {
            path: staged.display().to_string(),
            source,
        })),
    };
    if let Err(e) = tokio::fs::remove_dir_all(&staging).await {
        tracing::debug!(error = %e, "a staged upload not cleared");
    }
    sent
}

/// Answer a creation or a change: the item it left, or none when it is gone, `pending` the
/// fields left as they are here; or why it failed.
fn answer_change(
    reply: &Reply<ChangeFn>,
    answer: Result<Option<Item>, Unchanged>,
    pending: Fields,
) {
    answer_fetched(reply, answer.map(|left| (left, false)), pending);
}

/// [`answer_change`], the item's contents fetched again when the answer says so: the worker's
/// differ from what the system holds.
fn answer_fetched(
    reply: &Reply<ChangeFn>,
    answer: Result<(Option<Item>, bool), Unchanged>,
    pending: Fields,
) {
    let error = match answer {
        Ok((left, fetch)) => {
            let item: Option<Retained<NSFileProviderItem>> =
                left.map(|item| ProtocolObject::from_retained(FileItem::new(item)));
            let item =
                item.as_ref().map_or(std::ptr::null_mut(), |i| Retained::as_ptr(i).cast_mut());
            reply.0.call((item, pending, Bool::new(fetch), std::ptr::null_mut()));
            return;
        }
        Err(Unchanged::Error(e)) => {
            tracing::info!(error = %e, "a change not made on the worker");
            ns_error(&e)
        }
        Err(Unchanged::Refused(error)) => error,
    };
    reply.0.call((
        std::ptr::null_mut(),
        Fields::empty(),
        Bool::NO,
        Retained::as_ptr(&error).cast_mut(),
    ));
}

/// Answer a deletion: done, or why not.
fn answer_deleted(reply: &Reply<dyn Fn(*mut NSError)>, error: Option<&NSError>) {
    reply.0.call((error.map_or(std::ptr::null_mut(), |e| std::ptr::from_ref(e).cast_mut()),));
}

/// The extension has no domain to act in.
fn no_domain() -> FilesError {
    FilesError::Unreachable("no domain".to_owned())
}

/// A fetch's progress in the units Finder's bar counts: the file's bytes, as the transfer
/// tells them, its total and how many landed. One unit, none of it landed, until the
/// transfer names its size, so the bar never runs past its end.
fn units(brought: &Brought) -> (i64, i64) {
    let total = i64::try_from(brought.total.max(1)).unwrap_or(i64::MAX);
    let landed = i64::try_from(brought.done).unwrap_or(i64::MAX).min(total);
    (total, if brought.total == 0 { 0 } else { landed })
}

/// A progress of one unit whose cancel stops transfer `xfer` of `domain`; a fetch's counts
/// the file's bytes once its transfer names them ([`units`]).
fn cancellable(domain: &Arc<Domain>, xfer: XferId) -> Retained<NSProgress> {
    let progress = NSProgress::discreteProgressWithTotalUnitCount(1);
    let stopping = Arc::clone(domain);
    let cancel = RcBlock::new(move || {
        let domain = Arc::clone(&stopping);
        let _started = spawn(async move { domain.cancel(xfer).await });
    });
    // SAFETY: Foundation rule: a progress's cancellation handler may be any block; it is
    // called once, on a queue of the progress's choosing, and holds only `Send` values.
    unsafe {
        progress.setCancellationHandler(Some(&cancel));
    }
    progress
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `FileItem` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "SloptyFilesItem"]
    #[ivars = Item]
    struct FileItem;

    impl FileItem {}

    unsafe impl NSObjectProtocol for FileItem {}

    unsafe impl NSFileProviderItemProtocol for FileItem {
        #[unsafe(method_id(itemIdentifier))]
        fn item_identifier(&self) -> Retained<NSString> {
            identifier(&self.ivars().id)
        }

        #[unsafe(method_id(parentItemIdentifier))]
        fn parent_item_identifier(&self) -> Retained<NSString> {
            identifier(&self.ivars().parent)
        }

        #[unsafe(method_id(filename))]
        fn name_in_folder(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().name)
        }

        #[unsafe(method_id(contentType))]
        fn content_type(&self) -> Retained<UTType> {
            let item = self.ivars();
            // SAFETY: UniformTypeIdentifiers rule: the core types are constant objects.
            let (folder, data) = unsafe { (UTTypeFolder, UTTypeData) };
            if item.folder {
                folder.retain()
            } else {
                Path::new(&item.name)
                    .extension()
                    .and_then(|ext| {
                        UTType::typeWithFilenameExtension(&NSString::from_str(&ext.to_string_lossy()))
                    })
                    .unwrap_or_else(|| data.retain())
            }
        }

        /// What Finder may do with it: read it; add to a folder; save a file in place, over
        /// the version it was opened at; rename, move and trash anything but the root.
        #[unsafe(method(capabilities))]
        fn allowed(&self) -> NSFileProviderItemCapabilities {
            type Can = NSFileProviderItemCapabilities;
            let item = self.ivars();
            // `AllowsAddingSubItems` is `AllowsWriting`'s bit: a folder's means adding to it.
            let mut can = Can::AllowsReading | Can::AllowsWriting;
            if item.id != item::ROOT {
                can |= Can::AllowsRenaming | Can::AllowsReparenting | Can::AllowsTrashing;
            }
            can
        }

        #[unsafe(method(fileSystemFlags))]
        fn file_system_flags(&self) -> NSFileProviderFileSystemFlags {
            let item = self.ivars();
            let mut flags = NSFileProviderFileSystemFlags::UserReadable
                | NSFileProviderFileSystemFlags::UserWritable;
            if item.folder {
                flags |= NSFileProviderFileSystemFlags::UserExecutable;
            }
            if item.hidden {
                flags |= NSFileProviderFileSystemFlags::Hidden;
            }
            flags
        }

        #[unsafe(method_id(documentSize))]
        fn document_size(&self) -> Option<Retained<NSNumber>> {
            let item = self.ivars();
            (!item.folder).then(|| NSNumber::new_u64(item.size))
        }

        #[unsafe(method_id(childItemCount))]
        fn child_item_count(&self) -> Option<Retained<NSNumber>> {
            self.ivars().children.map(NSNumber::new_u32)
        }

        #[unsafe(method_id(contentModificationDate))]
        fn content_modification_date(&self) -> Option<Retained<NSDate>> {
            let ms = self.ivars().modified_ms;
            #[expect(clippy::cast_precision_loss, reason = "a date to the millisecond fits an f64")]
            let seconds = ms as f64 / 1000.0;
            (ms > 0).then(|| NSDate::dateWithTimeIntervalSince1970(seconds))
        }

        #[unsafe(method_id(itemVersion))]
        fn item_version(&self) -> Retained<NSFileProviderItemVersion> {
            let item = self.ivars();
            let content = NSData::with_bytes(&item.content_version());
            let metadata = NSData::with_bytes(&item.metadata_version());
            // SAFETY: FileProvider rule: a version is made from any two pieces of data.
            unsafe {
                NSFileProviderItemVersion::initWithContentVersion_metadataVersion(
                    NSFileProviderItemVersion::alloc(),
                    &content,
                    &metadata,
                )
            }
        }
    }
);

impl FileItem {
    fn new(item: Item) -> Retained<Self> {
        let this = Self::alloc().set_ivars(item);
        // SAFETY: `NSObject`'s `init` takes nothing and returns the same object.
        unsafe { msg_send![super(this), init] }
    }
}

/// What an enumerator lists: a folder's items, or the working set's changes (`None`).
struct Listing {
    domain: Arc<Domain>,
    folder: Option<String>,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Enumerator` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "SloptyFilesEnumerator"]
    #[ivars = Listing]
    struct Enumerator;

    impl Enumerator {}

    unsafe impl NSObjectProtocol for Enumerator {}

    unsafe impl NSFileProviderEnumerator for Enumerator {
        #[unsafe(method(invalidate))]
        fn ended(&self) {}

        /// A folder's items a page at a time, each the worker's own page of it
        /// ([`crate::pages`]): the system's first page asks for the folder's first, and each
        /// page the extension hands back asks for the one after it. The working set lists none,
        /// since every folder the system holds is listed on its own and its changes come
        /// through [`Self::enumerate_changes`].
        #[unsafe(method(enumerateItemsForObserver:startingAtPage:))]
        fn enumerate_items(
            &self,
            observer: &ProtocolObject<dyn NSFileProviderEnumerationObserver>,
            page: &NSData,
        ) {
            let observer = Held(observer.retain());
            let Listing { domain, folder } = self.ivars();
            let Some(folder) = folder.clone() else {
                // SAFETY: FileProvider rule: an observer is told its enumeration ended once.
                unsafe {
                    observer.get().finishEnumeratingUpToPage(None);
                }
                return;
            };
            let from = Cursor::decode(&page.to_vec());
            let domain = Arc::clone(domain);
            let started = spawn(async move {
                match domain.list_page(&folder, from.as_ref()).await {
                    Ok(Page { items, next }) => {
                        let items = items_array(items);
                        let next = next.map(|next| NSData::with_bytes(&next.encode()));
                        // SAFETY: FileProvider rule: an observer takes the items of one page,
                        // then is told where the next starts, or that there is none, from any
                        // thread.
                        unsafe {
                            observer.get().didEnumerateItems(&items);
                        }
                        // SAFETY: as above.
                        unsafe {
                            observer.get().finishEnumeratingUpToPage(next.as_deref());
                        }
                    }
                    Err(e) => {
                        tracing::info!(%folder, error = %e, "a folder not listed");
                        // SAFETY: as above, an enumeration that ends in an error.
                        unsafe {
                            observer.get().finishEnumeratingWithError(&ns_error(&e));
                        }
                    }
                }
            });
            if !started {
                tracing::error!("no runtime to list a folder on");
            }
        }

        /// The changes since `anchor`, all at once.
        #[unsafe(method(enumerateChangesForObserver:fromSyncAnchor:))]
        fn enumerate_changes(
            &self,
            observer: &ProtocolObject<dyn NSFileProviderChangeObserver>,
            anchor: &NSData,
        ) {
            match self.ivars().domain.since(&anchor.to_vec()) {
                Ok((changes, read)) => {
                    let (mut updated, mut deleted) = (Vec::new(), Vec::new());
                    for change in changes {
                        match change {
                            Change::Updated(item) => updated.push(item),
                            Change::Deleted(id) => deleted.push(identifier(&id)),
                        }
                    }
                    let (updated, deleted) =
                        (items_array(updated), NSArray::from_retained_slice(&deleted));
                    let read = NSData::with_bytes(&read);
                    // SAFETY: FileProvider rule: a change observer takes the updates and the
                    // deletions, then is told up to which anchor they go, once.
                    unsafe {
                        observer.didUpdateItems(&updated);
                    }
                    // SAFETY: as above.
                    unsafe {
                        observer.didDeleteItemsWithIdentifiers(&deleted);
                    }
                    // SAFETY: as above.
                    unsafe {
                        observer.finishEnumeratingChangesUpToSyncAnchor_moreComing(&read, false);
                    }
                }
                Err(_expired) => {
                    // SAFETY: FileProvider rule: the domain is a constant string, and a code
                    // of its own with no user info makes a valid error.
                    let error = unsafe {
                        NSError::errorWithDomain_code_userInfo(
                            NSFileProviderErrorDomain,
                            NSFileProviderErrorCode::SyncAnchorExpired.0,
                            None,
                        )
                    };
                    // SAFETY: FileProvider rule: an expired anchor ends the enumeration with
                    // that error, and the system lists everything again.
                    unsafe {
                        observer.finishEnumeratingWithError(&error);
                    }
                }
            }
        }

        #[unsafe(method(currentSyncAnchorWithCompletionHandler:))]
        fn current_sync_anchor(&self, completion: &DynBlock<dyn Fn(*mut NSData)>) {
            let anchor = NSData::with_bytes(&self.ivars().domain.anchor());
            completion.call((Retained::as_ptr(&anchor).cast_mut(),));
        }
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    /// Finder's bar counts a fetch's bytes once its transfer names the file's size, and never
    /// runs past its end; before that it is one unit, none of it done.
    #[test]
    fn a_fetch_s_progress_counts_the_file_s_bytes() {
        let brought = |done, total| Brought { done, total, ..Brought::default() };
        assert_eq!(units(&brought(0, 0)), (1, 0), "no size yet");
        assert_eq!(units(&brought(0, 6_000_000)), (6_000_000, 0));
        assert_eq!(units(&brought(2_500_000, 6_000_000)), (6_000_000, 2_500_000));
        assert_eq!(units(&brought(7_000_000, 6_000_000)), (6_000_000, 6_000_000), "capped");
        assert_eq!(units(&brought(u64::MAX, u64::MAX)), (i64::MAX, i64::MAX));
    }
}

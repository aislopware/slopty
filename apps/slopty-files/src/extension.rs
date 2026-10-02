//! The face the system calls: the replicated File Provider extension, its items and its
//! enumerators, each a thin Objective-C class over a [`Domain`].
//!
//! The system calls in on queues of its own and hands completion handlers that may be called
//! from any thread, so each call copies its handler, starts the work on the extension's own
//! runtime and returns at once. The domain is read-only: an item may be read and evicted, and
//! a change made in Finder is refused.

use std::ffi::{CString, c_char, c_int};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{
    AnyThread as _, ClassType as _, DefinedClass as _, Message as _, define_class, msg_send,
};
use objc2_file_provider::{
    NSFileProviderChangeObserver, NSFileProviderCreateItemOptions, NSFileProviderDeleteItemOptions,
    NSFileProviderDomain, NSFileProviderEnumerating, NSFileProviderEnumerationObserver,
    NSFileProviderEnumerator, NSFileProviderErrorCode, NSFileProviderErrorDomain,
    NSFileProviderFileSystemFlags, NSFileProviderItem, NSFileProviderItemCapabilities,
    NSFileProviderItemFields, NSFileProviderItemProtocol, NSFileProviderItemVersion,
    NSFileProviderManager, NSFileProviderModifyItemOptions, NSFileProviderReplicatedExtension,
    NSFileProviderRequest, NSFileProviderRootContainerItemIdentifier,
    NSFileProviderWorkingSetContainerItemIdentifier,
};
use objc2_foundation::{
    NSArray, NSCocoaErrorDomain, NSData, NSDate, NSError, NSFeatureUnsupportedError, NSNumber,
    NSProgress, NSString, NSURL, NSUserCancelledError,
};
use objc2_uniform_type_identifiers::{UTType, UTTypeData, UTTypeFolder};
use slopty_client::xfer::XferError;
use slopty_core::{WorkerId, XferId};
use tokio::runtime::Runtime;

use crate::changes::Change;
use crate::domain::{Domain, Signal};
use crate::item::{self, Item};
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
    let provider = |code: NSFileProviderErrorCode| {
        // SAFETY: FileProvider rule: the domain is a constant string, and a code of its own
        // with no user info makes a valid error.
        unsafe { NSError::errorWithDomain_code_userInfo(NSFileProviderErrorDomain, code.0, None) }
    };
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
        FilesError::Transfer(_) => provider(NSFileProviderErrorCode::CannotSynchronize),
    }
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
            _template: &NSFileProviderItem,
            _fields: NSFileProviderItemFields,
            _contents: Option<&NSURL>,
            _options: NSFileProviderCreateItemOptions,
            _request: &NSFileProviderRequest,
            completion: &DynBlock<
                dyn Fn(*mut NSFileProviderItem, NSFileProviderItemFields, Bool, *mut NSError),
            >,
        ) -> Retained<NSProgress> {
            refuse_change(completion);
            finished()
        }

        #[unsafe(method_id(modifyItem:baseVersion:changedFields:contents:options:request:completionHandler:))]
        fn modify_item(
            &self,
            _item: &NSFileProviderItem,
            _version: &NSFileProviderItemVersion,
            _fields: NSFileProviderItemFields,
            _contents: Option<&NSURL>,
            _options: NSFileProviderModifyItemOptions,
            _request: &NSFileProviderRequest,
            completion: &DynBlock<
                dyn Fn(*mut NSFileProviderItem, NSFileProviderItemFields, Bool, *mut NSError),
            >,
        ) -> Retained<NSProgress> {
            refuse_change(completion);
            finished()
        }

        #[unsafe(method_id(deleteItemWithIdentifier:baseVersion:options:request:completionHandler:))]
        fn delete_item(
            &self,
            _identifier: &NSString,
            _version: &NSFileProviderItemVersion,
            _options: NSFileProviderDeleteItemOptions,
            _request: &NSFileProviderRequest,
            completion: &DynBlock<dyn Fn(*mut NSError)>,
        ) -> Retained<NSProgress> {
            let error = cocoa(NSFeatureUnsupportedError);
            completion.call((Retained::as_ptr(&error).cast_mut(),));
            finished()
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
        let progress = NSProgress::discreteProgressWithTotalUnitCount(1);
        let (Some(domain), Some(temporary)) = (self.domain(), self.temporary()) else {
            let error = ns_error(&FilesError::Unreachable("no domain".to_owned()));
            reply.0.call((
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                Retained::as_ptr(&error).cast_mut(),
            ));
            return finished();
        };
        let xfer = XferId::new();
        let stopping = Arc::clone(&domain);
        let cancel = RcBlock::new(move || {
            let domain = Arc::clone(&stopping);
            let _started = spawn(async move { domain.cancel(xfer).await });
        });
        // SAFETY: Foundation rule: a progress's cancellation handler may be any block; it
        // is called once, on a queue of the progress's choosing, and holds only `Send`
        // values.
        unsafe {
            progress.setCancellationHandler(Some(&cancel));
        }
        let done = Retained::clone(&progress);
        let id = id_of(identifier);
        let started = spawn(async move {
            let into = temporary.join(xfer.to_string());
            let fetched = match tokio::fs::create_dir_all(&into).await {
                Ok(()) => domain.fetch(&id, &into, xfer).await,
                Err(e) => Err(FilesError::Transfer(XferError::Local {
                    path: into.display().to_string(),
                    source: e,
                })),
            };
            answer_contents(&reply, fetched);
            done.setCompletedUnitCount(1);
        });
        if !started {
            tracing::error!("no runtime to fetch a file on");
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

/// Refuse a change made in Finder: the domain is read-only.
fn refuse_change(
    completion: &DynBlock<
        dyn Fn(*mut NSFileProviderItem, NSFileProviderItemFields, Bool, *mut NSError),
    >,
) {
    let error = cocoa(NSFeatureUnsupportedError);
    completion.call((
        std::ptr::null_mut(),
        NSFileProviderItemFields::empty(),
        Bool::NO,
        Retained::as_ptr(&error).cast_mut(),
    ));
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

        #[unsafe(method(capabilities))]
        fn allowed(&self) -> NSFileProviderItemCapabilities {
            NSFileProviderItemCapabilities::AllowsReading
        }

        #[unsafe(method(fileSystemFlags))]
        fn file_system_flags(&self) -> NSFileProviderFileSystemFlags {
            let item = self.ivars();
            let mut flags = NSFileProviderFileSystemFlags::UserReadable;
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

        /// A folder's items, all in one page; the working set lists none, since every folder
        /// the system holds is listed on its own and its changes come through
        /// [`Self::enumerate_changes`].
        #[unsafe(method(enumerateItemsForObserver:startingAtPage:))]
        fn enumerate_items(
            &self,
            observer: &ProtocolObject<dyn NSFileProviderEnumerationObserver>,
            _page: &NSData,
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
            let domain = Arc::clone(domain);
            let started = spawn(async move {
                match domain.list(&folder).await {
                    Ok(items) => {
                        let items = items_array(items);
                        // SAFETY: FileProvider rule: an observer takes the items of one page,
                        // then is told the enumeration ended, from any thread.
                        unsafe {
                            observer.get().didEnumerateItems(&items);
                        }
                        // SAFETY: as above.
                        unsafe {
                            observer.get().finishEnumeratingUpToPage(None);
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

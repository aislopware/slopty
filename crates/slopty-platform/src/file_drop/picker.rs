//! The Files and Photos pickers on iOS: files or photos picked there go up to a tile, and a
//! worker's file brought down here is saved in Files.
//!
//! "Save to Files" asks for a folder first ([`choose_folder`]): the worker's file then comes
//! down straight into it, as a download with its progress and its stop, while the folder's
//! security scope is held ([`Scoped`]), so nothing is fetched whole before the person sees it
//! move and nothing is stored twice.
//!
//! The picker in import mode hands over copies in the app's own temporary directory, which are
//! moved into a landing and go on as a drop's files do ([`Dropped`]), so the upload deletes them
//! when it ends. One picker shows at a time: a new one ends the last as dismissed.
//!
//! The Photos picker runs out of the app's process and needs no access to the library: what is
//! picked comes as item providers, loaded as a drop's are, in the most compatible form (a HEIC
//! photo as a JPEG, which every agent reads).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{
    DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, extern_class,
    extern_methods, msg_send,
};
use objc2_foundation::{NSArray, NSObject, NSString, NSURL};
use objc2_photos_ui::{
    PHPickerConfiguration, PHPickerConfigurationAssetRepresentationMode, PHPickerResult,
    PHPickerViewControllerDelegate,
};
use objc2_ui_kit::{
    UIApplication, UIDocumentPickerDelegate, UIDocumentPickerViewController, UIResponder,
    UIViewController, UIWindowScene,
};

use super::{Dropped, Landing, Waiting, arrive, root};

/// The type every file conforms to, packages included.
const ITEM_UTI: &str = "public.item";

/// The type of a folder.
const FOLDER_UTI: &str = "public.folder";

/// What a picker was shown for.
enum Purpose {
    /// Files picked are landed and handed here.
    Import(Rc<dyn Fn(Dropped)>),
    /// Called once with the folder chosen, entered; with none when the picker was dismissed.
    Folder(RefCell<Option<FolderSink>>),
}

/// Where a chosen folder goes ([`choose_folder`]).
type FolderSink = Box<dyn FnOnce(Option<Scoped>)>;

/// A folder the person chose in Files, reachable while this is held.
///
/// A folder outside the app's sandbox is reached only inside its security scope: entered as it
/// is chosen, and left when this drops, so every way a download into it ends (done, failed,
/// stopped, its link lost, the view gone) leaves it exactly once.
#[derive(Debug)]
pub struct Scoped {
    url: Retained<NSURL>,
    path: PathBuf,
    /// Whether entering the scope succeeded, and so must be left: a folder inside the app's
    /// own container needs no scope, and Foundation says so by returning `false`.
    entered: bool,
}

impl Scoped {
    /// Enter `url`'s security scope; none for a URL that is not a file's.
    fn enter(url: Retained<NSURL>) -> Option<Self> {
        let path = PathBuf::from(url.path()?.to_string());
        // SAFETY: Foundation's security-scoped URL rule (`NSURL.h`,
        // `startAccessingSecurityScopedResource`): a URL the document picker hands over in open
        // mode is reached only between this call and a balancing
        // `stopAccessingSecurityScopedResource`, made once by `Drop` when this returned `true`.
        let entered = unsafe { url.startAccessingSecurityScopedResource() };
        tracing::info!(path = %path.display(), entered, "a folder chosen in Files");
        Some(Self { url, path, entered })
    }

    /// Where the folder is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scoped {
    fn drop(&mut self) {
        if self.entered {
            // SAFETY: the same rule: one `stopAccessingSecurityScopedResource` for the one
            // `startAccessingSecurityScopedResource` that returned `true` in `enter`.
            unsafe {
                self.url.stopAccessingSecurityScopedResource();
            }
        }
    }
}

struct Ivars {
    purpose: Purpose,
}

thread_local! {
    /// The delegate of the picker showing: the picker holds it weakly.
    static SHOWN: RefCell<Option<Retained<Picker>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Picker` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyFilesPicker"]
    #[ivars = Ivars]
    struct Picker;

    unsafe impl NSObjectProtocol for Picker {}

    unsafe impl UIDocumentPickerDelegate for Picker {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn picked(&self, _controller: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
            self.end(urls.to_vec());
            let _released = SHOWN.take().map(Retained::autorelease_ptr);
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn cancelled(&self, _controller: &UIDocumentPickerViewController) {
            self.end(Vec::new());
            let _released = SHOWN.take().map(Retained::autorelease_ptr);
        }
    }
);

impl std::fmt::Debug for Picker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Picker")
    }
}

impl Picker {
    /// The picker is done with `urls`: the files or the folder picked; none when it was
    /// dismissed.
    fn end(&self, urls: Vec<Retained<NSURL>>) {
        match &self.ivars().purpose {
            Purpose::Import(sink) => {
                let paths = urls.iter().filter_map(|url| url.path());
                if let Some(dropped) = land(paths.map(|p| PathBuf::from(p.to_string())).collect()) {
                    sink(dropped);
                }
            }
            Purpose::Folder(sink) => {
                if let Some(sink) = sink.borrow_mut().take() {
                    sink(urls.into_iter().next().and_then(Scoped::enter));
                }
            }
        }
    }
}

/// The picked copies moved into a landing of their own; none when nothing was picked.
fn land(paths: Vec<PathBuf>) -> Option<Dropped> {
    if paths.is_empty() {
        return None;
    }
    let mut landing = match Landing::new(&root(), paths.len(), (0.0, 0.0)) {
        Ok(landing) => landing,
        Err(e) => {
            tracing::warn!(error = %e, "no directory for picked files");
            return None;
        }
    };
    let dir = landing.dir().to_path_buf();
    tracing::info!(files = paths.len(), dir = %dir.display(), "files picked");
    let mut dropped = None;
    for from in paths {
        let name = from.file_name().map(|n| n.to_string_lossy().into_owned());
        let outcome = match name {
            Some(name) => arrive(&dir, &from, &name),
            None => Err((None, "not a file".to_owned())),
        };
        dropped = landing.resolve(outcome);
    }
    dropped
}

extern_class!(
    /// PhotosUI's picker, a view controller on iOS: `objc2-photos-ui` binds it on macOS alone,
    /// as an `NSViewController`.
    #[unsafe(super(UIViewController, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PHPickerViewController"]
    #[derive(Debug)]
    struct PhotoPicker;
);

impl PhotoPicker {
    extern_methods!(
        /// A picker that shows as `configuration` says.
        #[unsafe(method(initWithConfiguration:))]
        #[unsafe(method_family = init)]
        fn init_with_configuration(
            this: Allocated<Self>,
            configuration: &PHPickerConfiguration,
        ) -> Retained<Self>;

        /// Who hears what was picked; held weakly.
        #[unsafe(method(setDelegate:))]
        #[unsafe(method_family = none)]
        fn set_delegate(
            &self,
            delegate: Option<&ProtocolObject<dyn PHPickerViewControllerDelegate>>,
        );
    );
}

/// Where a picker's files go once landed.
type PickedSink = Rc<dyn Fn(Dropped)>;

/// The Photos picker's delegate: what is picked goes to the sink its id names.
struct PhotosIvars {
    sink: u64,
}

thread_local! {
    /// The Photos picker's delegate while it shows: the picker holds it weakly.
    static PHOTOS: RefCell<Option<Retained<Photos>>> = const { RefCell::new(None) };
    /// The sinks of photos still loading. A sink is not `Send`, so it waits here on the main
    /// thread while the photos load off it.
    static SINKS: RefCell<Waiting<PickedSink>> = RefCell::default();
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Photos` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyPhotosPicker"]
    #[ivars = PhotosIvars]
    struct Photos;

    unsafe impl NSObjectProtocol for Photos {}

    unsafe impl PHPickerViewControllerDelegate for Photos {}

    impl Photos {
        /// PhotosUI's `picker:didFinishPicking:`, called on a pick and on Cancel alike, with
        /// nothing in the second case. The picker does not dismiss itself.
        #[unsafe(method(picker:didFinishPicking:))]
        fn finished(&self, picker: &PhotoPicker, results: &NSArray<PHPickerResult>) {
            picker.dismissViewControllerAnimated_completion(true, None);
            // SAFETY: PhotosUI rule: a result's provider is valid for as long as it is held.
            let providers: Vec<_> = results.iter().map(|r| unsafe { r.itemProvider() }).collect();
            let sink = self.ivars().sink;
            if providers.is_empty() {
                let _dismissed = SINKS.with(|s| s.borrow_mut().take(sink));
            } else {
                tracing::info!(photos = providers.len(), "photos picked");
                let done: Arc<dyn Fn(Dropped) + Send + Sync> = Arc::new(move |dropped| {
                    dispatch2::DispatchQueue::main().exec_async(move || hand(sink, dropped));
                });
                super::ios::load(providers, (0.0, 0.0), &done);
            }
            let _released = PHOTOS.take().map(Retained::autorelease_ptr);
        }
    }
);

/// Hand the photos loaded for `sink` to it, on the main thread; once only.
fn hand(sink: u64, dropped: Dropped) {
    match SINKS.with(|s| s.borrow_mut().take(sink)) {
        Some(sink) => sink(dropped),
        None => {
            if let Some(landing) = &dropped.landing {
                super::discard(landing);
            }
        }
    }
}

/// Show the Photos picker for photos and videos to upload; `sink` gets them landed, as a drop's.
///
/// Main thread only: `false` off it, or with no window to show it over.
pub fn import_photos(sink: Rc<dyn Fn(Dropped)>) -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let Some(top) = top_controller(mtm) else {
        tracing::warn!("no window to show the Photos picker over");
        return false;
    };
    let id = SINKS.with(|s| s.borrow_mut().wait(sink));
    // SAFETY: PhotosUI rule: a configuration made with no photo library shows every asset
    // through the picker's own process, so the app is granted nothing.
    let configuration = unsafe { PHPickerConfiguration::new() };
    // SAFETY: PhotosUI rule: a setter of a configuration not yet handed to a picker; 0 is no
    // limit.
    unsafe {
        configuration.setSelectionLimit(0);
    }
    let compatible = PHPickerConfigurationAssetRepresentationMode::Compatible;
    // SAFETY: PhotosUI rule: as above, and one of the mode's own values.
    unsafe {
        configuration.setPreferredAssetRepresentationMode(compatible);
    }
    let picker = PhotoPicker::init_with_configuration(PhotoPicker::alloc(mtm), &configuration);
    let this = Photos::alloc(mtm).set_ivars(PhotosIvars { sink: id });
    // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
    let delegate: Retained<Photos> = unsafe { msg_send![super(this), init] };
    picker.set_delegate(Some(ProtocolObject::from_ref(&*delegate)));
    if let Some(last) = PHOTOS.replace(Some(delegate)) {
        let _replaced = SINKS.with(|s| s.borrow_mut().take(last.ivars().sink));
    }
    top.presentViewController_animated_completion(&picker, true, None);
    true
}

/// Show the picker for files to upload; `sink` gets them landed, as a drop's.
///
/// Main thread only: `false` off it, or with no window to show it over.
pub fn import(sink: Rc<dyn Fn(Dropped)>) -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let types = NSArray::from_retained_slice(&[NSString::from_str(ITEM_UTI)]);
    #[expect(
        deprecated,
        reason = "`initForOpeningContentTypes:asCopy:` needs UniformTypeIdentifiers"
    )]
    let picker = UIDocumentPickerViewController::initWithDocumentTypes_inMode(
        UIDocumentPickerViewController::alloc(mtm),
        &types,
        objc2_ui_kit::UIDocumentPickerMode::Import,
    );
    picker.setAllowsMultipleSelection(true);
    present(mtm, &picker, Purpose::Import(sink))
}

/// Show the picker for a folder to save into; `sink` gets it, its scope entered, or none once
/// the picker is dismissed. It is called once either way.
///
/// Main thread only: `false` off it, or with no window to show it over, and `sink` has not run.
pub fn choose_folder(sink: FolderSink) -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let types = NSArray::from_retained_slice(&[NSString::from_str(FOLDER_UTI)]);
    #[expect(deprecated, reason = "`initForOpeningContentTypes:` needs UniformTypeIdentifiers")]
    let picker = UIDocumentPickerViewController::initWithDocumentTypes_inMode(
        UIDocumentPickerViewController::alloc(mtm),
        &types,
        objc2_ui_kit::UIDocumentPickerMode::Open,
    );
    present(mtm, &picker, Purpose::Folder(RefCell::new(Some(sink))))
}

/// Show `picker` over whatever is showing, its delegate kept until it is done.
fn present(
    mtm: MainThreadMarker,
    picker: &UIDocumentPickerViewController,
    purpose: Purpose,
) -> bool {
    let Some(top) = top_controller(mtm) else {
        tracing::warn!("no window to show the Files picker over");
        return false;
    };
    if let Some(last) = SHOWN.take() {
        last.end(Vec::new());
    }
    let this = Picker::alloc(mtm).set_ivars(Ivars { purpose });
    // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
    let delegate: Retained<Picker> = unsafe { msg_send![super(this), init] };
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    SHOWN.set(Some(delegate));
    top.presentViewController_animated_completion(picker, true, None);
    true
}

/// The controller on top in the key window of the app's first window scene that has one.
fn top_controller(mtm: MainThreadMarker) -> Option<Retained<UIViewController>> {
    let scenes = UIApplication::sharedApplication(mtm).connectedScenes();
    let window = scenes
        .iter()
        .filter_map(|scene| scene.downcast::<UIWindowScene>().ok())
        .find_map(|scene| scene.keyWindow())?;
    let mut top = window.rootViewController()?;
    while let Some(next) = top.presentedViewController() {
        top = next;
    }
    Some(top)
}

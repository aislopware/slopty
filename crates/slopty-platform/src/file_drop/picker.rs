//! The Files and Photos pickers on iOS: files or photos picked there go up to a tile, and a
//! worker's file brought down here is saved in Files.
//!
//! The picker in import mode hands over copies in the app's own temporary directory, which are
//! moved into a landing and go on as a drop's files do ([`Dropped`]), so the upload deletes them
//! when it ends. In export mode it copies the files it is given to wherever the person chooses;
//! the caller deletes its own once the picker is done. One picker shows at a time: a new one
//! ends the last as dismissed.
//!
//! The Photos picker runs out of the app's process and needs no access to the library: what is
//! picked comes as item providers, loaded as a drop's are, in the most compatible form (a HEIC
//! photo as a JPEG, which every agent reads).

use std::cell::RefCell;
use std::path::PathBuf;
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

/// What a picker was shown for.
enum Purpose {
    /// Files picked are landed and handed here.
    Import(Rc<dyn Fn(Dropped)>),
    /// Called once the files are saved or the picker is dismissed.
    Export(RefCell<Option<Box<dyn FnOnce()>>>),
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
            let paths =
                urls.iter().filter_map(|url| url.path()).map(|p| PathBuf::from(p.to_string()));
            self.end(paths.collect());
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
    /// The picker is done with `paths`: the files picked, or where the copies went.
    fn end(&self, paths: Vec<PathBuf>) {
        match &self.ivars().purpose {
            Purpose::Import(sink) => {
                if let Some(dropped) = land(paths) {
                    sink(dropped);
                }
            }
            Purpose::Export(done) => {
                tracing::info!(saved = paths.len(), "saved to Files");
                if let Some(done) = done.borrow_mut().take() {
                    done();
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

/// Show the picker saving copies of `paths` where the person chooses; `done` runs once it is
/// done or dismissed, for the caller to delete its own.
///
/// Main thread only: `false` off it, or with no window to show it over, and `done` has not run.
pub fn export(paths: &[PathBuf], done: Box<dyn FnOnce()>) -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let urls: Vec<Retained<NSURL>> = paths
        .iter()
        .map(|p| NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy())))
        .collect();
    let picker = UIDocumentPickerViewController::initForExportingURLs_asCopy(
        UIDocumentPickerViewController::alloc(mtm),
        &NSArray::from_retained_slice(&urls),
        true,
    );
    present(mtm, &picker, Purpose::Export(RefCell::new(Some(done))))
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

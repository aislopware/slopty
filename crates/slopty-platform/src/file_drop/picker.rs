//! The Files picker on iOS: files picked there go up to a tile, and a worker's file brought down
//! here is saved there.
//!
//! The picker in import mode hands over copies in the app's own temporary directory, which are
//! moved into a landing and go on as a drop's files do ([`Dropped`]), so the upload deletes them
//! when it ends. In export mode it copies the files it is given to wherever the person chooses;
//! the caller deletes its own once the picker is done. One picker shows at a time: a new one
//! ends the last as dismissed.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSArray, NSObject, NSString, NSURL};
use objc2_ui_kit::{
    UIApplication, UIDocumentPickerDelegate, UIDocumentPickerViewController, UIViewController,
    UIWindowScene,
};

use super::{Dropped, Landing, arrive, root};

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

//! A worker's files handed to another app from iOS: a row dragged out of an iPad's folder tile,
//! or a file saved to Files.
//!
//! Nothing comes down until the other side asks. A drag carries an item provider per file whose
//! file representation is registered, not written (`registerFileRepresentation…`): when the
//! drop's receiver loads it, the file comes down from the worker into a directory of its own
//! under [`outbox`], on a thread of its own, and the provider hands its URL over, as the Mac's
//! file promise is kept on its own queue. A drag's directories go once the receiver has the
//! files (or the drop is cancelled); any a run left behind are swept when the next one starts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The type a file's representation is registered and loaded as: every file conforms to it.
pub const DATA_UTI: &str = "public.data";
/// A folder's, which does not conform to [`DATA_UTI`].
pub const FOLDER_UTI: &str = "public.folder";

/// Brings a worker's file or folder into this directory and says where it landed; the error is
/// for a person.
pub type Fetch = Arc<dyn Fn(&Path) -> Result<PathBuf, String> + Send + Sync>;

/// One of a worker's files offered to another app.
#[derive(Clone)]
pub struct Offer {
    /// The file's name, as the receiver will first try it.
    pub name: String,
    /// Whether it is a folder, which is offered as one.
    pub folder: bool,
    /// Brings it here.
    pub fetch: Fetch,
}

impl std::fmt::Debug for Offer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offer")
            .field("name", &self.name)
            .field("folder", &self.folder)
            .finish_non_exhaustive()
    }
}

impl Offer {
    /// The worker's file at `path`, named by its last part; none for the root, which has no
    /// name to give it.
    #[must_use]
    pub fn of(path: &str, folder: bool, fetch: Fetch) -> Option<Self> {
        let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path);
        (!name.is_empty()).then(|| Self { name: name.to_owned(), folder, fetch })
    }

    /// The type its representation is registered as.
    #[must_use]
    pub const fn type_identifier(&self) -> &'static str {
        if self.folder { FOLDER_UTI } else { DATA_UTI }
    }

    /// Bring it into a fresh directory under `root`; where it landed. A fetch that failed
    /// leaves nothing behind.
    ///
    /// # Errors
    ///
    /// When no directory can be made, or the fetch fails.
    pub fn fetch_under(&self, root: &Path) -> Result<PathBuf, String> {
        let dir = super::fresh_dir(root).map_err(|e| e.to_string())?;
        let landed = (self.fetch)(&dir);
        if landed.as_ref().is_ok_and(|path| path.parent() == Some(&*dir)) {
            return landed;
        }
        super::discard_in(root, &dir);
        landed.and_then(|path| Err(format!("{} landed outside its directory", path.display())))
    }
}

/// Where files brought down to be handed to another app wait: a directory per fetch, named as a
/// drop's landing is, under the system's temporary directory.
#[must_use]
pub fn outbox() -> PathBuf {
    std::env::temp_dir().join("slopty-out")
}

/// Delete what [`Offer::fetch_under`] brought into the [`outbox`], once it has been handed
/// over. Nothing outside the outbox is touched.
pub fn discard(landed: &Path) {
    if let Some(dir) = landed.parent() {
        super::discard_in(&outbox(), dir);
    }
}

/// The one entry at the top of what a download into `into` landed: the file, or the folder
/// with everything in it.
///
/// # Errors
///
/// When nothing landed, or it landed outside `into`.
pub fn landed_top(landed: &[PathBuf], into: &Path) -> Result<PathBuf, String> {
    let first = landed.first().ok_or_else(|| "nothing arrived".to_owned())?;
    let top = first
        .strip_prefix(into)
        .ok()
        .and_then(|rel| rel.components().next())
        .ok_or_else(|| "arrived outside its directory".to_owned())?;
    Ok(into.join(top))
}

#[cfg(target_os = "ios")]
pub use ios::{Pick, offer};

#[cfg(target_os = "ios")]
mod ios {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::ptr::NonNull;
    use std::rc::Rc;
    use std::sync::Arc;

    use block2::{DynBlock, RcBlock};
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObjectProtocol, ProtocolObject};
    use objc2::{DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_foundation::{
        NSArray, NSError, NSItemProvider, NSItemProviderFileOptions,
        NSItemProviderRepresentationVisibility, NSObject, NSProgress, NSString, NSURL,
    };
    use objc2_ui_kit::{
        UIDragDropSession as _, UIDragInteraction, UIDragInteractionDelegate, UIDragItem,
        UIDragPreviewParameters, UIDragPreviewTarget, UIDragSession, UIDropOperation, UIImage,
        UIImageView, UIInteraction as _, UITargetedDragPreview, UIView, UIViewContentMode,
    };
    use parking_lot::Mutex;

    use super::{FOLDER_UTI, Offer, outbox};

    /// The worker files under a point of the host view (its points from the top left).
    pub type Pick = Rc<dyn Fn(f64, f64) -> Vec<Offer>>;

    /// The side of the icon a lifted file is shown as, in points: a Files icon's.
    const ICON: f64 = 56.0;
    /// Drags whose end has not been heard; the oldest are let go past this many.
    const LIVE_DRAGS: usize = 16;

    /// The directories fetched for one drag.
    type Fetched = Arc<Mutex<Vec<PathBuf>>>;
    /// What a file representation's load handler is given to call once the file is here.
    type Done = DynBlock<dyn Fn(*mut NSURL, Bool, *mut NSError)>;
    /// Each host view's source and its interaction.
    type Offering = (usize, Retained<Source>, Retained<UIDragInteraction>);

    thread_local! {
        /// Each host view's source and interaction, by the view's address.
        static OFFERING: RefCell<Vec<Offering>> = RefCell::default();
    }

    struct Ivars {
        pick: Pick,
        /// Each drag begun, by its session's address, and what was fetched for it.
        drags: RefCell<Vec<(usize, Fetched)>>,
    }

    define_class!(
        // SAFETY:
        // - `NSObject` has no subclassing requirements.
        // - `Source` does not implement `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SloptyDragOutSource"]
        #[ivars = Ivars]
        struct Source;

        unsafe impl NSObjectProtocol for Source {}

        unsafe impl UIDragInteractionDelegate for Source {
            #[unsafe(method_id(dragInteraction:itemsForBeginningSession:))]
            fn items(
                &self,
                interaction: &UIDragInteraction,
                session: &ProtocolObject<dyn UIDragSession>,
            ) -> Retained<NSArray<UIDragItem>> {
                self.begin(interaction, session)
            }

            #[unsafe(method_id(dragInteraction:previewForLiftingItem:session:))]
            fn lift_preview(
                &self,
                interaction: &UIDragInteraction,
                item: &UIDragItem,
                session: &ProtocolObject<dyn UIDragSession>,
            ) -> Option<Retained<UITargetedDragPreview>> {
                self.preview(interaction, item, session)
            }

            #[unsafe(method(dragInteraction:sessionAllowsMoveOperation:))]
            fn allows_move(
                &self,
                _interaction: &UIDragInteraction,
                _session: &ProtocolObject<dyn UIDragSession>,
            ) -> bool {
                false
            }

            #[unsafe(method(dragInteraction:sessionDidTransferItems:))]
            fn transferred(
                &self,
                _interaction: &UIDragInteraction,
                session: &ProtocolObject<dyn UIDragSession>,
            ) {
                self.finish(session);
            }

            #[unsafe(method(dragInteraction:session:didEndWithOperation:))]
            fn ended(
                &self,
                _interaction: &UIDragInteraction,
                session: &ProtocolObject<dyn UIDragSession>,
                operation: UIDropOperation,
            ) {
                // A drop that took the files hears of their transfer after this.
                if operation == UIDropOperation::Cancel || operation == UIDropOperation::Forbidden {
                    self.finish(session);
                }
            }
        }
    );

    impl std::fmt::Debug for Source {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Source")
        }
    }

    impl Source {
        /// The items of a drag starting at the session's touch: none when no worker file is
        /// there, and the touch goes on as the app's own.
        fn begin(
            &self,
            interaction: &UIDragInteraction,
            session: &ProtocolObject<dyn UIDragSession>,
        ) -> Retained<NSArray<UIDragItem>> {
            let Some(view) = interaction.view() else { return NSArray::new() };
            let at = session.locationInView(&view);
            let offers = (self.ivars().pick)(at.x, at.y);
            if offers.is_empty() {
                return NSArray::new();
            }
            let mtm = self.mtm();
            let fetched = Fetched::default();
            let items: Vec<Retained<UIDragItem>> =
                offers.into_iter().map(|offer| item(mtm, offer, &fetched)).collect();
            tracing::info!(files = items.len(), "drag out");
            let mut drags = self.ivars().drags.borrow_mut();
            drags.push((session_key(session), fetched));
            if drags.len() > LIVE_DRAGS {
                drags.remove(0);
            }
            NSArray::from_retained_slice(&items)
        }

        /// The lifted file as its kind's icon under the finger, not a picture of the whole
        /// window, which is the view the interaction is on.
        fn preview(
            &self,
            interaction: &UIDragInteraction,
            item: &UIDragItem,
            session: &ProtocolObject<dyn UIDragSession>,
        ) -> Option<Retained<UITargetedDragPreview>> {
            let mtm = self.mtm();
            let view = interaction.view()?;
            let folder =
                item.itemProvider().hasRepresentationConformingToTypeIdentifier_fileOptions(
                    &NSString::from_str(FOLDER_UTI),
                    NSItemProviderFileOptions::empty(),
                );
            let symbol = NSString::from_str(if folder { "folder.fill" } else { "doc.fill" });
            let image = UIImage::systemImageNamed(&symbol);
            let icon = UIImageView::initWithImage(UIImageView::alloc(mtm), image.as_deref());
            icon.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(ICON, ICON)));
            icon.setContentMode(UIViewContentMode::ScaleAspectFit);
            let target = UIDragPreviewTarget::initWithContainer_center(
                UIDragPreviewTarget::alloc(mtm),
                &view,
                session.locationInView(&view),
            );
            Some(UITargetedDragPreview::initWithView_parameters_target(
                UITargetedDragPreview::alloc(mtm),
                &icon,
                &UIDragPreviewParameters::new(mtm),
                &target,
            ))
        }

        /// The drag is over for its files: what came down for it goes, off the main thread.
        fn finish(&self, session: &ProtocolObject<dyn UIDragSession>) {
            let key = session_key(session);
            let mut drags = self.ivars().drags.borrow_mut();
            let Some(at) = drags.iter().position(|(k, _)| *k == key) else { return };
            let (_, fetched) = drags.remove(at);
            let dirs = std::mem::take(&mut *fetched.lock());
            if dirs.is_empty() {
                return;
            }
            let spawned =
                std::thread::Builder::new().name("slopty-drag-out-done".to_owned()).spawn(
                    move || dirs.iter().for_each(|dir| super::super::discard_in(&outbox(), dir)),
                );
            if let Err(e) = spawned {
                tracing::warn!(error = %e, "discard a drag's files");
            }
        }
    }

    /// A session told apart from the others by its address, which it keeps while it lives.
    fn session_key(session: &ProtocolObject<dyn UIDragSession>) -> usize {
        std::ptr::from_ref(session).addr()
    }

    /// A provider's completion handler, called once the file is here or cannot be.
    struct Completion(RcBlock<dyn Fn(*mut NSURL, Bool, *mut NSError)>);

    #[expect(
        clippy::non_send_fields_in_send_ty,
        reason = "the block is only called, which Foundation allows from any thread"
    )]
    // SAFETY: Foundation rule: an item provider's completion handler may be called from any
    // thread; the block is a heap copy, immutable once made, and called once.
    unsafe impl Send for Completion {}
    // SAFETY: as above; nothing mutates the block.
    unsafe impl Sync for Completion {}

    impl Completion {
        fn call(&self, landed: Result<PathBuf, String>) {
            match landed {
                Ok(path) => {
                    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
                    self.0.call((
                        Retained::as_ptr(&url).cast_mut(),
                        Bool::NO,
                        std::ptr::null_mut(),
                    ));
                }
                Err(why) => {
                    tracing::warn!(%why, "a file dragged out did not come down");
                    let domain = NSString::from_str("com.aislopware.slopty");
                    // SAFETY: Foundation rule: any domain string, any code, and a nil user
                    // info make a valid error.
                    let error = unsafe { NSError::errorWithDomain_code_userInfo(&domain, 1, None) };
                    self.0.call((
                        std::ptr::null_mut(),
                        Bool::NO,
                        Retained::as_ptr(&error).cast_mut(),
                    ));
                }
            }
        }
    }

    /// The drag item of `offer`: its file representation brings it down only when a receiver
    /// loads it.
    fn item(mtm: MainThreadMarker, offer: Offer, fetched: &Fetched) -> Retained<UIDragItem> {
        let provider = NSItemProvider::new();
        provider.setSuggestedName(Some(&NSString::from_str(&offer.name)));
        let uti = NSString::from_str(offer.type_identifier());
        let offer = Arc::new(offer);
        let fetched = Arc::clone(fetched);
        let load = RcBlock::new(move |done: NonNull<Done>| {
            // SAFETY: Foundation rule: the completion handler is a valid block for the
            // call; its copy may be kept and called once, later.
            let done = Arc::new(Completion(unsafe { done.as_ref() }.copy()));
            let progress = NSProgress::discreteProgressWithTotalUnitCount(1);
            let (offer, fetched) = (Arc::clone(&offer), Arc::clone(&fetched));
            let (reply, counted) = (Arc::clone(&done), Retained::clone(&progress));
            let spawned =
                std::thread::Builder::new().name("slopty-drag-out".to_owned()).spawn(move || {
                    let landed = offer.fetch_under(&outbox());
                    if let Some(dir) = landed.as_ref().ok().and_then(|p| p.parent()) {
                        fetched.lock().push(dir.to_path_buf());
                    }
                    counted.setCompletedUnitCount(1);
                    reply.call(landed);
                });
            if let Err(e) = spawned {
                done.call(Err(e.to_string()));
            }
            Retained::autorelease_ptr(progress)
        });
        // SAFETY: Foundation rule: a type identifier, no options, visible to every process,
        // and a load handler that calls its completion once, from any thread, which it may
        // be: it holds only `Send` values.
        unsafe {
            provider
                .registerFileRepresentationForTypeIdentifier_fileOptions_visibility_loadHandler(
                    &uti,
                    NSItemProviderFileOptions::empty(),
                    NSItemProviderRepresentationVisibility::All,
                    &load,
                );
        }
        UIDragItem::initWithItemProvider(UIDragItem::alloc(mtm), &provider)
    }

    /// Let worker files be dragged out of `host` to other apps: a touch held on one lifts it,
    /// and `pick` names the files under it.
    ///
    /// `host` is the view a GPUI window draws into (its `raw_window_handle` handle). Once per
    /// view: `false` off the main thread or when already done. UIKit enables drags on an iPad
    /// and not on an iPhone, whose drags stay in one app.
    pub fn offer(host: NonNull<c_void>, pick: Pick) -> bool {
        let key = host.as_ptr().addr();
        if OFFERING.with(|o| o.borrow().iter().any(|(h, ..)| *h == key)) {
            return false;
        }
        let Some(mtm) = MainThreadMarker::new() else { return false };
        // SAFETY: `raw_window_handle`'s UIKit rule: the handle is a live `UIView` of the
        // window, valid while the window is; the app offers drags once, for the window it
        // keeps for its whole run.
        let Some(host) = (unsafe { Retained::retain(host.as_ptr().cast::<UIView>()) }) else {
            return false;
        };
        let this = Source::alloc(mtm).set_ivars(Ivars { pick, drags: RefCell::default() });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
        let source: Retained<Source> = unsafe { msg_send![super(this), init] };
        let interaction = UIDragInteraction::initWithDelegate(
            UIDragInteraction::alloc(mtm),
            ProtocolObject::from_ref(&*source),
        );
        host.addInteraction(ProtocolObject::from_ref(&*interaction));
        tracing::debug!(enabled = interaction.isEnabled(), "files may be dragged out");
        OFFERING.with(|o| o.borrow_mut().push((key, source, interaction)));
        let spawned = std::thread::Builder::new()
            .name("slopty-out-sweep".to_owned())
            .spawn(|| super::super::sweep(&outbox()));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "sweep old drags");
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nothing() -> Fetch {
        Arc::new(|_into: &Path| Err("not here".to_owned()))
    }

    /// An item provider offers a worker's file under its own name, as data, and a folder as a
    /// folder; the root has no name to offer.
    #[test]
    fn an_offer_is_named_by_its_path_and_typed_by_its_kind() {
        let file = Offer::of("/w/proj/report.pdf", false, nothing()).unwrap();
        assert_eq!((file.name.as_str(), file.type_identifier()), ("report.pdf", DATA_UTI));
        let folder = Offer::of("/w/proj/src/", true, nothing()).unwrap();
        assert_eq!((folder.name.as_str(), folder.type_identifier()), ("src", FOLDER_UTI));
        assert!(Offer::of("/", true, nothing()).is_none());
        assert!(Offer::of("", false, nothing()).is_none());
    }

    /// A fetch lands in a directory of its own, and a failed one, or one that lands elsewhere,
    /// leaves nothing behind; a download's top entry is the file or folder asked for.
    #[test]
    fn a_fetch_lands_in_a_directory_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("out");
        let fetch: Fetch = Arc::new(|into: &Path| {
            std::fs::create_dir_all(into.join("src/deep")).map_err(|e| e.to_string())?;
            std::fs::write(into.join("src/deep/a.rs"), b"fn a() {}").map_err(|e| e.to_string())?;
            landed_top(&[into.join("src/deep/a.rs")], into)
        });
        let offer = Offer::of("/w/src", true, fetch).unwrap();
        let first = offer.fetch_under(&root).unwrap();
        let second = offer.fetch_under(&root).unwrap();
        assert_eq!(first.file_name(), Some("src".as_ref()));
        assert_ne!(first.parent(), second.parent(), "a directory per fetch");
        assert!(first.join("deep/a.rs").is_file());

        let failed = Offer::of("/w/gone.txt", false, nothing()).unwrap();
        assert_eq!(failed.fetch_under(&root).unwrap_err(), "not here");
        let astray: Fetch = Arc::new(|_into: &Path| Ok(PathBuf::from("/tmp/elsewhere")));
        Offer::of("/w/a", false, astray).unwrap().fetch_under(&root).unwrap_err();
        let dirs = std::fs::read_dir(&root).unwrap().count();
        assert_eq!(dirs, 2, "only the two that landed");

        super::super::discard_in(&root, first.parent().unwrap());
        assert!(!first.exists());
        assert!(landed_top(&[], &root).is_err(), "nothing arrived");
        assert!(landed_top(&[PathBuf::from("/x/y")], &root).is_err(), "outside");
    }
}

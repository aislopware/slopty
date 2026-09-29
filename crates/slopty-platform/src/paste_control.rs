//! The system's paste button (`UIPasteControl`), shown where the app offers a paste into a
//! remote tile or a terminal.
//!
//! Reading another app's clipboard on iOS asks the person each time, unless a paste they made
//! does the reading (`pasteboard::Pasteboard::reads_ask`). A tap on this button is such a
//! paste: UIKit draws it, knows the tap was the person's, and hands its target what was on
//! the clipboard with no prompt. A hardware keyboard's ⌘V keeps the key path it has.
//!
//! The button is a subview of the GPUI view, placed by the UI in the view's points
//! ([`PasteButton::show`]) like the browser tile's page. It is enabled while the clipboard holds
//! anything ([`accepted_types`]), since clipboard sync carries every type between Apple ends.
//! What a tap pastes comes back to the UI's callback on the main thread as one [`Pasted`]: every
//! item, each with the representations that travel and are clipboard-sized. Video and audio are
//! left out (a Photos video is gigabytes), an item's pictures are loaded once, in the first
//! format it offers (every other is a conversion made on demand), and a representation past
//! [`MAX_PASTED_REP`], or past [`MAX_PASTED`] for the whole paste, is dropped without being copied.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSArray, NSData, NSError, NSItemProvider, NSObject, NSObjectProtocol, NSString,
};
use objc2_ui_kit::{
    UIButtonConfigurationCornerStyle, UIColor, UIPasteConfiguration,
    UIPasteConfigurationSupporting, UIPasteControl, UIPasteControlConfiguration,
    UIPasteControlDisplayMode, UIResponder, UIView,
};
use parking_lot::Mutex;
use slopty_proto::transfer::ClipFormat;

use crate::pasteboard::{self, Memory};
use crate::web::Frame;

/// The types the button takes: every format by name, then `public.item`, which every type
/// conforms to, so any clipboard enables it.
pub fn accepted_types() -> impl Iterator<Item = &'static str> {
    ClipFormat::ALL.into_iter().map(pasteboard::uti_of).chain(std::iter::once("public.item"))
}

/// The largest representation a tap keeps.
pub const MAX_PASTED_REP: u64 = 64 << 20;

/// The most one tap keeps, over every representation of every item.
pub const MAX_PASTED: u64 = 128 << 20;

/// Whether a type a provider registered is loaded by a tap: one that travels in an offer.
fn travels(uti: &str) -> bool {
    pasteboard::format_of(uti).is_some() || pasteboard::carried(uti)
}

/// Whether `uti` is, or conforms to, `to` in the system's type tree; `false` for a type the
/// system does not know.
fn conforms(uti: &str, to: &str) -> bool {
    let Some(class) = AnyClass::get(c"UTType") else { return false };
    let named = |id: &str| -> Option<Retained<NSObject>> {
        let id = NSString::from_str(id);
        // SAFETY: UniformTypeIdentifiers rule (`UTType.h`): `+typeWithIdentifier:` takes any
        // string and answers nil for one the system does not know, which `Option` covers.
        unsafe { msg_send![class, typeWithIdentifier: &*id] }
    };
    let (Some(uti), Some(to)) = (named(uti), named(to)) else { return false };
    // SAFETY: UniformTypeIdentifiers rule (`UTType.h`): `-conformsToType:` takes a non-nil
    // type and answers a `BOOL`.
    unsafe { msg_send![&*uti, conformsToType: &*to] }
}

/// The types of `provider` a tap loads, in its own order: those that travel, but no video or
/// audio, and of its pictures only the first.
fn loaded(provider: &NSItemProvider) -> Vec<String> {
    let mut picture = false;
    accepted(provider)
        .into_iter()
        .filter(|t| !conforms(t, "public.audiovisual-content"))
        .filter(|t| !conforms(t, "public.image") || !std::mem::replace(&mut picture, true))
        .collect()
}

/// What one tap pasted: every item, each with the representations that travel, in the order
/// the copying app ranked them (richest first).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pasted {
    /// Each item's `(type, bytes)` pairs.
    pub items: Vec<Vec<(String, Vec<u8>)>>,
}

impl Pasted {
    /// The bytes of the first item's representation of `uti`.
    #[must_use]
    pub fn data(&self, uti: &str) -> Option<&[u8]> {
        self.items.first()?.iter().find(|(t, _)| t == uti).map(|(_, b)| b.as_slice())
    }

    /// The text pasted, when there is any.
    #[must_use]
    pub fn text(&self) -> Option<String> {
        self.data(pasteboard::TEXT_UTI).map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// The paste as a pasteboard whose reads never ask, for the paths that read one (a clipboard
    /// offer to a worker, a picture pasted into a shell).
    #[must_use]
    pub fn board(&self) -> Memory {
        let board = Memory::default();
        let items: Vec<Vec<(&str, &[u8])>> = self
            .items
            .iter()
            .map(|reps| reps.iter().map(|(t, b)| (t.as_str(), b.as_slice())).collect())
            .collect();
        let items: Vec<&[(&str, &[u8])]> = items.iter().map(Vec::as_slice).collect();
        board.copy_items(&items);
        board
    }
}

/// How the button is drawn: the system draws it, from these alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    /// Icon, label, or both.
    pub mode: Mode,
    /// Its icon and label, as red, green, blue and alpha from 0 to 1.
    pub foreground: [f64; 4],
    /// Its fill.
    pub background: [f64; 4],
    /// Its corners' radius, in points.
    pub corner_radius: f64,
}

/// What the button shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The paste icon alone, for a key bar.
    Icon,
    /// The icon and "Paste".
    IconAndLabel,
    /// "Paste" alone.
    Label,
}

impl Mode {
    const fn display(self) -> UIPasteControlDisplayMode {
        match self {
            Self::Icon => UIPasteControlDisplayMode::IconOnly,
            Self::IconAndLabel => UIPasteControlDisplayMode::IconAndLabel,
            Self::Label => UIPasteControlDisplayMode::LabelOnly,
        }
    }
}

/// Where a button's pastes go, on the main thread.
type Sink = Rc<dyn Fn(Pasted)>;

thread_local! {
    /// Each button's sink, by its target's address.
    static SINKS: RefCell<Vec<(usize, Sink)>> = RefCell::default();
}

/// Hand a finished paste to button `key`'s sink, on the main thread.
fn deliver(key: usize, pasted: Pasted) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        let sink =
            SINKS.with(|s| s.borrow().iter().find(|(k, _)| *k == key).map(|(_, s)| Rc::clone(s)));
        if let Some(sink) = sink {
            sink(pasted);
        } else {
            tracing::debug!(items = pasted.items.len(), "a paste for a button that is gone");
        }
    });
}

define_class!(
    // SAFETY:
    // - `UIResponder` may be subclassed on the main thread; this one only adds the paste methods of
    //   `UIPasteConfigurationSupporting`, which `UIResponder` already adopts.
    // - `Target` does not implement `Drop`.
    #[unsafe(super(UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyPasteTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl UIPasteConfigurationSupporting for Target {
        /// The person tapped the button: load every representation of the accepted types.
        #[unsafe(method(pasteItemProviders:))]
        fn paste_item_providers(&self, providers: &NSArray<NSItemProvider>) {
            load(self.key(), providers);
        }

        #[unsafe(method(canPasteItemProviders:))]
        fn can_paste_item_providers(&self, providers: &NSArray<NSItemProvider>) -> bool {
            providers.iter().any(|p| !accepted(&p).is_empty())
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `UIResponder`'s `init` on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }

    fn key(&self) -> usize {
        std::ptr::from_ref(self) as usize
    }
}

/// The types `provider` has that travel, in its own order.
fn accepted(provider: &NSItemProvider) -> Vec<String> {
    provider
        .registeredTypeIdentifiers()
        .iter()
        .map(|t| t.to_string())
        .filter(|t| travels(t))
        .collect()
}

/// A paste being loaded: one slot per representation, with its item, filled as each arrives.
#[derive(Debug)]
struct Loading {
    slots: Vec<Option<(usize, String, Vec<u8>)>>,
    items: usize,
    left: usize,
    /// Bytes kept so far, against [`MAX_PASTED`].
    kept: u64,
}

impl Loading {
    /// What arrived, item by item.
    fn pasted(&mut self) -> Pasted {
        let mut items = vec![Vec::new(); self.items];
        for (item, uti, bytes) in self.slots.iter_mut().filter_map(Option::take) {
            if let Some(reps) = items.get_mut(item) {
                reps.push((uti, bytes));
            }
        }
        items.retain(|reps: &Vec<(String, Vec<u8>)>| !reps.is_empty());
        Pasted { items }
    }
}

/// Load each representation of `providers` that travels, then deliver them to button `key`.
fn load(key: usize, providers: &NSArray<NSItemProvider>) {
    let wanted: Vec<(usize, Retained<NSItemProvider>, String)> = providers
        .iter()
        .enumerate()
        .flat_map(|(n, p)| loaded(&p).into_iter().map(move |t| (n, Retained::clone(&p), t)))
        .collect();
    if wanted.is_empty() {
        deliver(key, Pasted::default());
        return;
    }
    let loading = Arc::new(Mutex::new(Loading {
        slots: vec![None; wanted.len()],
        items: providers.count(),
        left: wanted.len(),
        kept: 0,
    }));
    for (slot, (item, provider, uti)) in wanted.into_iter().enumerate() {
        let loading = Arc::clone(&loading);
        let name = uti.clone();
        let done = RcBlock::new(move |data: *mut NSData, _error: *mut NSError| {
            // SAFETY: Foundation rule: the data is nil or a valid `NSData` for the duration of
            // the completion handler; its bytes are copied out here, when they are kept.
            let data = unsafe { data.as_ref() };
            let mut loading = loading.lock();
            let size = data.map_or(0, |d| u64::try_from(d.length()).unwrap_or(u64::MAX));
            let fits = size <= MAX_PASTED_REP && loading.kept.saturating_add(size) <= MAX_PASTED;
            if let Some(data) = data.filter(|_| fits) {
                loading.kept = loading.kept.saturating_add(size);
                if let Some(place) = loading.slots.get_mut(slot) {
                    *place = Some((item, name.clone(), data.to_vec()));
                }
            } else if data.is_some() {
                tracing::debug!(uti = %name, size, "a pasted representation too big to keep");
            }
            loading.left = loading.left.saturating_sub(1);
            if loading.left == 0 {
                let pasted = loading.pasted();
                drop(loading);
                deliver(key, pasted);
            }
        });
        // SAFETY: Foundation rule: `loadDataRepresentationForTypeIdentifier:completionHandler:`
        // takes a type the provider registered and calls the block exactly once, on a queue of
        // its choosing, which the block may run on: it holds only `Send` data behind a lock.
        let _progress = unsafe {
            provider.loadDataRepresentationForTypeIdentifier_completionHandler(
                &NSString::from_str(&uti),
                &done,
            )
        };
    }
}

const fn rect(frame: Frame) -> CGRect {
    CGRect::new(CGPoint::new(frame.x, frame.y), CGSize::new(frame.w.max(0.0), frame.h.max(0.0)))
}

fn color(rgba: [f64; 4]) -> Retained<UIColor> {
    let [r, g, b, a] = rgba;
    UIColor::colorWithRed_green_blue_alpha(r, g, b, a)
}

/// The system's paste button in a GPUI view. Main thread only; dropping it takes it away.
pub struct PasteButton {
    control: Retained<UIPasteControl>,
    /// The control's target, which it holds weakly.
    target: Retained<Target>,
}

impl std::fmt::Debug for PasteButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasteButton").field("shown", &self.shown()).finish_non_exhaustive()
    }
}

impl PasteButton {
    /// A hidden paste button drawn as `style`, inside `host`, the `UIView` a GPUI window draws
    /// into (its `raw_window_handle` UIKit handle). Each tap's paste goes to `on_paste`, on the
    /// main thread. `None` off the main thread.
    #[must_use]
    pub fn new(host: NonNull<c_void>, style: Style, on_paste: Rc<dyn Fn(Pasted)>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        // SAFETY: `raw_window_handle`'s UIKit rule: the handle is a live `UIView` of the
        // window, valid while the window is; the UI drops the button before its window.
        let host: Retained<UIView> = unsafe { Retained::retain(host.as_ptr().cast::<UIView>()) }?;
        let config = UIPasteControlConfiguration::new(mtm);
        config.setDisplayMode(style.mode.display());
        config.setCornerStyle(UIButtonConfigurationCornerStyle::Fixed);
        config.setCornerRadius(style.corner_radius);
        config.setBaseForegroundColor(Some(&color(style.foreground)));
        config.setBaseBackgroundColor(Some(&color(style.background)));
        let control = UIPasteControl::initWithConfiguration(UIPasteControl::alloc(mtm), &config);
        let target = Target::new(mtm);
        let types: Vec<Retained<NSString>> = accepted_types().map(NSString::from_str).collect();
        let accepts = UIPasteConfiguration::initWithAcceptableTypeIdentifiers(
            UIPasteConfiguration::alloc(mtm),
            &NSArray::from_retained_slice(&types),
        );
        target.setPasteConfiguration(Some(&accepts));
        control.setTarget(Some(ProtocolObject::from_ref(&*target)));
        control.setHidden(true);
        host.addSubview(&control);
        SINKS.with(|s| s.borrow_mut().push((target.key(), on_paste)));
        Some(Self { control, target })
    }

    /// Show the button at `frame`, in the GPUI view's points from its top left.
    pub fn show(&self, frame: Frame) {
        self.control.setFrame(rect(frame));
        self.control.setHidden(false);
    }

    /// Take the button out of sight.
    pub fn hide(&self) {
        self.control.setHidden(true);
    }

    /// Whether the button is shown.
    #[must_use]
    pub fn shown(&self) -> bool {
        !self.control.isHidden()
    }

    /// Whether a tap would paste: the clipboard holds a type the button takes. The system
    /// dims the button otherwise.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.control.isEnabled()
    }
}

impl Drop for PasteButton {
    fn drop(&mut self) {
        let key = self.target.key();
        SINKS.with(|s| s.borrow_mut().retain(|(k, _)| *k != key));
        self.control.setTarget(None);
        self.control.removeFromSuperview();
    }
}

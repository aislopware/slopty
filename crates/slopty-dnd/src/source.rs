//! The helper's drag source, for a drop from the client into an app on the worker.
//!
//! The worker asks for it at the point the client's drag entered. It is a window of a few points
//! there, and the worker's injector presses into it through the HID tap: its view gets a real
//! `mouseDown:` and begins a session with that event, the drag the drag manager then carries
//! wherever the worker moves the pointer (P0 (1)). Once the session begins the window stops
//! taking the mouse, a failed drag does not slide back, and only Copy is allowed, so nothing on
//! the client is ever moved. Its image is clear: the person follows the client's own.

use std::cell::RefCell;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObjectProtocol, ProtocolObject};
use objc2::{
    AllocAnyThread as _, DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class,
    msg_send,
};
use objc2_app_kit::{
    NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource,
    NSEvent, NSImage, NSResponder, NSView, NSWindow,
};
use objc2_foundation::{NSArray, NSObject, NSPoint, NSRect, NSSize};

use crate::items::{Item, Provide, Writers};
use crate::window;

/// The source window's side, in points.
pub const SIDE: f64 = 8.0;

/// What the source tells the helper: global points, from the main display's top left.
pub trait SourceEvents {
    /// The session began at `at`.
    fn began(&self, at: (f64, f64));
    /// The drag manager took the session a step on, to `at`.
    fn moved(&self, at: (f64, f64));
    /// The session ended at `at` with `operation` (`NSDragOperation` bits; 0 when nothing
    /// took the drop).
    fn ended(&self, operation: u64, at: (f64, f64));
}

struct Ivars {
    /// What the next press drags.
    pending: RefCell<Option<Writers>>,
    /// What the last drag carried: its promises answer until the next drag, since a target may
    /// read after the drop.
    live: RefCell<Option<Writers>>,
    events: Box<dyn SourceEvents>,
}

define_class!(
    // SAFETY:
    // - `NSView` may be subclassed; the overrides keep its contracts (mouse handling that
    //   begins a drag as AppKit's documentation shows, a dragging source).
    // - `SourceView` does not implement `Drop`.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyDragSourceView"]
    #[ivars = Ivars]
    struct SourceView;

    impl SourceView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.begin(event);
        }
    }

    unsafe impl NSObjectProtocol for SourceView {}

    unsafe impl NSDraggingSource for SourceView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingSession:willBeginAtPoint:))]
        fn will_begin(&self, _session: &NSDraggingSession, at: NSPoint) {
            self.ivars().events.began(window::global(at));
        }

        #[unsafe(method(draggingSession:movedToPoint:))]
        fn moved(&self, _session: &NSDraggingSession, at: NSPoint) {
            self.ivars().events.moved(window::global(at));
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(&self, _session: &NSDraggingSession, at: NSPoint, operation: NSDragOperation) {
            if let Some(window) = self.window() {
                window.orderOut(None);
                window.setIgnoresMouseEvents(false);
            }
            self.ivars().events.ended(u64::try_from(operation.0).unwrap_or(0), window::global(at));
        }
    }
);

impl SourceView {
    fn new(mtm: MainThreadMarker, events: Box<dyn SourceEvents>) -> Retained<Self> {
        let ivars = Ivars { pending: RefCell::new(None), live: RefCell::new(None), events };
        let this = Self::alloc(mtm).set_ivars(ivars);
        let frame = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize { width: SIDE, height: SIDE },
        };
        // SAFETY: `NSView`'s designated initialiser on a freshly allocated instance.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// Begin the session from the press, as a view begins one from its mouse handling.
    fn begin(&self, event: &NSEvent) {
        let Some(writers) = self.ivars().pending.take() else { return };
        let at = self.convertPoint_fromView(event.locationInWindow(), None);
        let image = clear();
        let items: Vec<Retained<NSDraggingItem>> = writers
            .writers()
            .iter()
            .map(|writer| {
                let item =
                    NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
                let frame = NSRect {
                    origin: NSPoint { x: at.x - SIDE / 2.0, y: at.y - SIDE / 2.0 },
                    size: NSSize { width: SIDE, height: SIDE },
                };
                // SAFETY: AppKit rule: the contents of a dragging frame may be an `NSImage`.
                unsafe {
                    item.setDraggingFrame_contents(frame, Some(&image));
                }
                item
            })
            .collect();
        let session = self.beginDraggingSessionWithItems_event_source(
            &NSArray::from_retained_slice(&items),
            event,
            ProtocolObject::from_ref(self),
        );
        session.setAnimatesToStartingPositionsOnCancelOrFail(false);
        self.ivars().live.replace(Some(writers));
        if let Some(window) = self.window() {
            window.setIgnoresMouseEvents(true);
        }
    }
}

impl std::fmt::Debug for SourceView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceView")
    }
}

/// A clear square: the drag's image on the worker, where nobody follows it.
fn clear() -> Retained<NSImage> {
    let draw = RcBlock::new(|_rect: NSRect| -> Bool { Bool::YES });
    NSImage::imageWithSize_flipped_drawingHandler(
        NSSize { width: SIDE, height: SIDE },
        false,
        &draw,
    )
}

/// The helper's drag source: a window of [`SIDE`] points, out of sight until placed.
#[derive(Debug)]
pub struct Source {
    window: Retained<NSWindow>,
    view: Retained<SourceView>,
}

impl Source {
    /// A source that tells `events` how its drags go.
    #[must_use]
    pub fn new(mtm: MainThreadMarker, events: Box<dyn SourceEvents>) -> Self {
        let window = window::square(mtm, SIDE);
        let view = SourceView::new(mtm, events);
        window.setContentView(Some(&view));
        Self { window, view }
    }

    /// Wait at the global point `at` with `items`, their promises answered by `provide`: the
    /// next press into the window drags them. The last drag's promises go.
    pub fn at(&self, at: (f64, f64), items: &[Item], provide: &Provide) {
        self.view.ivars().live.replace(None);
        self.view.ivars().pending.replace(Some(Writers::new(items, provide)));
        self.window.setIgnoresMouseEvents(false);
        window::place(&self.window, at, SIDE);
    }

    /// Its window's number (`CGWindowID`), which the worker keeps above the window it raises.
    #[must_use]
    pub fn window_number(&self) -> isize {
        self.window.windowNumber()
    }

    /// Out of sight, with nothing to drag.
    pub fn stop(&self) {
        self.view.ivars().pending.replace(None);
        self.window.orderOut(None);
    }
}

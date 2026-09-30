//! An application that starts drags, for the drag-and-drop spikes (`tests/spikes.rs`), which is
//! its parent and the only process that posts to it. In the drop-in spikes it stands for the
//! worker's drag helper; in the drag-out ones, for an app on the worker that a person drags from.
//!
//! It opens one window, grey, its content at `--at x,y,w,h` (global points from the main
//! display's top left), at `--level normal|floating|popup`, borderless or with a title bar
//! (`--titled 1`). Its view answers `acceptsFirstMouse:`
//! with `--first-mouse 1|0` and begins a dragging session from the press (`--begin down`), from
//! the first drag of the button (`--begin dragged`, as apps do), or never (`--begin none`). The
//! session drags one item per `--file <path>` (its file URL), per `--promise <name>:<bytes>:<ms>`
//! (a file promise whose file, `<bytes>` long, is written `<ms>` after a destination calls it in,
//! saying `promise wrote=<path>`), and per `--text <s>`, each drawn as
//! a 64-point square, solid magenta (`--image magenta`) or clear (`--image clear`); the source
//! allows `--mask copy|all` outside the app, and with `--ignore 1` the window stops taking the
//! mouse once the session has begun, as the helper's does.
//!
//! It writes `ready pid=<pid> window=<CGWindowID>`, then a line for each press, drag and release
//! AppKit takes (`event`, with its event number), for each session callback (`began`, `moved`
//! when the point changes, `ended` with the operation), and `cursor class=<c>` each time the
//! system cursor changes class, read as the worker reads it (`slopty_capture::read_cursor`) and
//! classed against AppKit's arrow, copy, link, not-allowed and closed-hand cursors by the pixels
//! that differ. It never becomes the active application and leaves when its stdin closes.

#[cfg(target_os = "macos")]
#[path = "child.rs"]
mod child;

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};
    use std::process::ExitCode;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObjectProtocol, ProtocolObject};
    use objc2::{
        AllocAnyThread as _, DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class,
        msg_send,
    };
    use objc2_app_kit::{
        NSColor, NSCursor, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
        NSDraggingSource, NSEvent, NSFilePromiseProvider, NSFilePromiseProviderDelegate, NSImage,
        NSImageRep, NSPasteboardWriting, NSRectFill, NSResponder, NSView,
    };
    use objc2_core_graphics::{CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo};
    use objc2_foundation::{
        NSArray, NSError, NSObject, NSOperationQueue, NSPoint, NSRect, NSRunLoop,
        NSRunLoopCommonModes, NSSize, NSString, NSTimer, NSURL,
    };
    use slopty_capture::{AlphaAt, Layout, bgra_premultiplied};

    use crate::child::{self, Args, say};

    /// When the view begins its session.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Begin {
        Down,
        Dragged,
        Never,
    }

    pub struct Ivars {
        begin: Begin,
        first_mouse: bool,
        files: Vec<String>,
        promises: Vec<Retained<PromiseWriter>>,
        texts: Vec<String>,
        magenta: bool,
        mask: NSDragOperation,
        ignore: bool,
        begun: Cell<bool>,
        /// Where the last `moved` line put the drag.
        last: Cell<(i64, i64)>,
    }

    define_class!(
        // SAFETY:
        // - `NSView` may be subclassed; the overrides keep its contracts (mouse handling that
        //   begins a drag as AppKit's documentation shows, a dragging source).
        // - `SourceView` does not implement `Drop`.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SloptySpikeDragSourceView"]
        #[ivars = Ivars]
        struct SourceView;

        impl SourceView {
            #[unsafe(method(acceptsFirstMouse:))]
            fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
                self.ivars().first_mouse
            }

            #[unsafe(method(mouseDown:))]
            fn mouse_down(&self, event: &NSEvent) {
                say(&format!("view down number={}", event.eventNumber()));
                if self.ivars().begin == Begin::Down {
                    self.start(event);
                }
            }

            #[unsafe(method(mouseDragged:))]
            fn mouse_dragged(&self, event: &NSEvent) {
                if self.ivars().begin == Begin::Dragged && !self.ivars().begun.get() {
                    say(&format!("view dragged number={}", event.eventNumber()));
                    self.start(event);
                }
            }

            #[unsafe(method(mouseUp:))]
            fn mouse_up(&self, event: &NSEvent) {
                say(&format!("view up number={}", event.eventNumber()));
            }
        }

        unsafe impl NSObjectProtocol for SourceView {}

        unsafe impl NSDraggingSource for SourceView {
            #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
            fn source_mask(
                &self,
                _session: &NSDraggingSession,
                context: NSDraggingContext,
            ) -> NSDragOperation {
                if context == NSDraggingContext::WithinApplication {
                    NSDragOperation::Copy
                } else {
                    self.ivars().mask
                }
            }

            #[unsafe(method(draggingSession:willBeginAtPoint:))]
            fn will_begin(&self, _session: &NSDraggingSession, at: NSPoint) {
                say(&format!("began x={:.0} y={:.0}", at.x, at.y));
            }

            #[unsafe(method(draggingSession:movedToPoint:))]
            fn moved(&self, _session: &NSDraggingSession, at: NSPoint) {
                #[expect(clippy::cast_possible_truncation, reason = "screen points")]
                let point = (at.x.round() as i64, at.y.round() as i64);
                if self.ivars().last.replace(point) != point {
                    say(&format!("moved x={} y={}", point.0, point.1));
                }
            }

            #[unsafe(method(draggingSession:endedAtPoint:operation:))]
            fn ended(&self, _session: &NSDraggingSession, at: NSPoint, operation: NSDragOperation) {
                say(&format!("ended op={} x={:.0} y={:.0}", operation.0, at.x, at.y));
            }
        }
    );

    pub struct PromiseIvars {
        name: String,
        bytes: usize,
        after: std::time::Duration,
        queue: Retained<NSOperationQueue>,
    }

    define_class!(
        // SAFETY:
        // - `NSObject` has no subclassing requirements.
        // - `PromiseWriter` does not implement `Drop`.
        #[unsafe(super(NSObject))]
        #[name = "SloptySpikePromiseWriter"]
        #[ivars = PromiseIvars]
        pub struct PromiseWriter;

        unsafe impl NSObjectProtocol for PromiseWriter {}

        unsafe impl NSFilePromiseProviderDelegate for PromiseWriter {
            #[unsafe(method_id(filePromiseProvider:fileNameForType:))]
            fn file_name(
                &self,
                _provider: &NSFilePromiseProvider,
                _file_type: &NSString,
            ) -> Retained<NSString> {
                NSString::from_str(&self.ivars().name)
            }

            #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
            fn write(
                &self,
                _provider: &NSFilePromiseProvider,
                url: &NSURL,
                done: &block2::DynBlock<dyn Fn(*mut NSError)>,
            ) {
                let ivars = self.ivars();
                #[expect(clippy::disallowed_methods, reason = "a test app's slow promise")]
                std::thread::sleep(ivars.after);
                let path = url.path().map(|p| p.to_string()).unwrap_or_default();
                let wrote = std::fs::write(&path, vec![0x3c; ivars.bytes]).is_ok();
                say(&format!("promise wrote={path} ok={}", u8::from(wrote)));
                done.call((std::ptr::null_mut(),));
            }

            #[unsafe(method_id(operationQueueForFilePromiseProvider:))]
            fn queue(&self, _provider: &NSFilePromiseProvider) -> Retained<NSOperationQueue> {
                self.ivars().queue.clone()
            }
        }
    );

    impl PromiseWriter {
        /// A promise of `name`, `bytes` long, written `after` it is called in, off the main
        /// thread.
        fn new(spec: &str) -> Option<Retained<Self>> {
            let mut parts = spec.rsplitn(3, ':');
            let after = std::time::Duration::from_millis(parts.next()?.parse().ok()?);
            let bytes = parts.next()?.parse().ok()?;
            let name = parts.next()?.to_owned();
            let this = Self::alloc().set_ivars(PromiseIvars {
                name,
                bytes,
                after,
                queue: NSOperationQueue::new(),
            });
            // SAFETY: `NSObject`'s `init` on a freshly allocated instance with its ivars set.
            Some(unsafe { msg_send![super(this), init] })
        }
    }

    impl std::fmt::Debug for PromiseWriter {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("PromiseWriter")
        }
    }

    impl SourceView {
        fn new(mtm: MainThreadMarker, frame: NSRect, ivars: Ivars) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(ivars);
            // SAFETY: `NSView`'s designated initialiser on a freshly allocated instance.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        /// Begin the session from `event`, as a view begins one from its mouse handling.
        fn start(&self, event: &NSEvent) {
            let ivars = self.ivars();
            ivars.begun.set(true);
            let writers: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = ivars
                .files
                .iter()
                .map(|path| {
                    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
                    ProtocolObject::from_retained(url)
                })
                .chain(ivars.promises.iter().map(|writer| {
                    let provider = NSFilePromiseProvider::initWithFileType_delegate(
                        NSFilePromiseProvider::alloc(),
                        &NSString::from_str("public.data"),
                        ProtocolObject::from_ref(&**writer),
                    );
                    ProtocolObject::from_retained(provider)
                }))
                .chain(
                    ivars
                        .texts
                        .iter()
                        .map(|text| ProtocolObject::from_retained(NSString::from_str(text))),
                )
                .collect();
            let at = self.convertPoint_fromView(event.locationInWindow(), None);
            let image = square(ivars.magenta);
            let items: Vec<Retained<NSDraggingItem>> = writers
                .iter()
                .map(|writer| {
                    let item =
                        NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
                    let frame = NSRect {
                        origin: NSPoint { x: at.x - 32.0, y: at.y - 32.0 },
                        size: NSSize { width: 64.0, height: 64.0 },
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
            say(&format!("session items={}", items.len()));
            if ivars.ignore
                && let Some(window) = self.window()
            {
                window.setIgnoresMouseEvents(true);
            }
        }
    }

    impl std::fmt::Debug for SourceView {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("SourceView")
        }
    }

    /// A 64-point square: solid magenta, which a picture of the screen can be searched for, or
    /// clear, as the helper's drag image is.
    fn square(magenta: bool) -> Retained<NSImage> {
        let draw = RcBlock::new(move |rect: NSRect| -> Bool {
            if magenta {
                NSColor::magentaColor().setFill();
                NSRectFill(rect);
            }
            Bool::YES
        });
        NSImage::imageWithSize_flipped_drawingHandler(
            NSSize { width: 64.0, height: 64.0 },
            false,
            &draw,
        )
    }

    /// Width, height and premultiplied BGRA pixels.
    type Pixels = (u16, u16, Vec<u8>);

    /// AppKit's cursors by name, read at one scale.
    type References = Vec<(&'static str, Pixels)>;

    /// A cursor's picture as `read_cursor` reads the system's: premultiplied BGRA at `scale`
    /// pixels a point, the scale of the display the window server draws the cursor on.
    fn pixels(cursor: &NSCursor, scale: u8) -> Option<Pixels> {
        let image = cursor.image();
        let points = image.size();
        #[expect(clippy::cast_possible_truncation, reason = "a cursor is a few hundred pixels")]
        let wanted = (points.width * f64::from(scale)).round() as isize;
        let reps = image.representations();
        let rep: Retained<NSImageRep> = reps
            .iter()
            .filter(|rep| rep.pixelsWide() >= wanted)
            .min_by_key(|rep| rep.pixelsWide())
            .or_else(|| reps.iter().max_by_key(|rep| rep.pixelsWide()))?;
        // SAFETY: a null proposed rect asks for the whole image at the representation's own
        // pixel size; no context and no hints is the documented way to read its pixels.
        let cg =
            unsafe { rep.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
        let some = Some(&*cg);
        let (alpha, premultiplied, opaque) = match CGImage::alpha_info(some) {
            CGImageAlphaInfo::PremultipliedLast => (AlphaAt::Last, true, false),
            CGImageAlphaInfo::PremultipliedFirst => (AlphaAt::First, true, false),
            CGImageAlphaInfo::Last => (AlphaAt::Last, false, false),
            CGImageAlphaInfo::First => (AlphaAt::First, false, false),
            CGImageAlphaInfo::NoneSkipLast => (AlphaAt::Last, true, true),
            CGImageAlphaInfo::NoneSkipFirst => (AlphaAt::First, true, true),
            _ => return None,
        };
        let layout = Layout {
            width: u16::try_from(CGImage::width(some)).ok()?,
            height: u16::try_from(CGImage::height(some)).ok()?,
            bytes_per_row: CGImage::bytes_per_row(some),
            little_endian: CGImage::byte_order_info(some) == CGImageByteOrderInfo::Order32Little,
            alpha,
            premultiplied,
            opaque,
        };
        let provider = CGImage::data_provider(some)?;
        let data = CGDataProvider::data(Some(&provider))?.to_vec();
        Some((layout.width, layout.height, bgra_premultiplied(&layout, &data)?))
    }

    /// How many pixels of two pictures of one size differ by more than a quarter in any byte.
    fn differing(a: &[u8], b: &[u8]) -> usize {
        a.as_chunks::<4>()
            .0
            .iter()
            .zip(b.as_chunks::<4>().0)
            .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 64))
            .count()
    }

    /// AppKit's arrow, copy, link, not-allowed and closed-hand cursors at `scale`.
    fn references(scale: u8) -> References {
        let references: References = [
            ("arrow", NSCursor::arrowCursor()),
            ("copy", NSCursor::dragCopyCursor()),
            ("link", NSCursor::dragLinkCursor()),
            ("notallowed", NSCursor::operationNotAllowedCursor()),
            ("closedhand", NSCursor::closedHandCursor()),
        ]
        .into_iter()
        .filter_map(|(name, cursor)| Some((name, pixels(&cursor, scale)?)))
        .collect();
        say(&format!(
            "references at {scale}x {}",
            references
                .iter()
                .map(|(name, (w, h, _))| format!("{name}={w}x{h}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        references
    }

    /// Say the system cursor's class each time it changes: the reference of its size with the
    /// fewest differing pixels, or `other`. The references are read at the scale of the picture
    /// the window server hands back, and again when that scale changes.
    fn watch_cursor() -> Retained<NSTimer> {
        let at_scale: RefCell<Option<(u8, References)>> = RefCell::new(None);
        let watch = RefCell::new(slopty_capture::CursorWatch::new());
        let last = RefCell::new(String::new());
        let tick = RcBlock::new(move |_timer| {
            let Some(shape) = watch.borrow_mut().poll() else { return };
            let mut at_scale = at_scale.borrow_mut();
            if at_scale.as_ref().is_none_or(|(scale, _)| *scale != shape.scale) {
                *at_scale = Some((shape.scale, references(shape.scale)));
            }
            let Some((_, references)) = at_scale.as_ref() else { return };
            let best = references
                .iter()
                .filter(|(_, (w, h, _))| (*w, *h) == (shape.w, shape.h))
                .map(|(name, (_, _, bgra))| (*name, differing(bgra, &shape.bgra)))
                .min_by_key(|(_, diff)| *diff);
            let class = match best {
                Some((name, diff)) if diff <= 8 => format!("{name} diff={diff}"),
                Some((name, diff)) => format!("other nearest={name} diff={diff}"),
                None => "other nearest=none".to_owned(),
            };
            let line = format!("cursor class={class} size={}x{}", shape.w, shape.h);
            if *last.borrow() != line {
                say(&line);
                last.replace(line);
            }
        });
        // SAFETY: AppKit rule: a repeating timer made here and added to this, the main, run
        // loop in the common modes, so it also fires while AppKit tracks the mouse.
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(0.02, true, &tick) };
        // SAFETY: AppKit rule: a timer is added to the run loop of the thread it fires on, this
        // one; the mode is a framework-provided constant string.
        unsafe {
            NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        }
        timer
    }

    pub fn run() -> ExitCode {
        let Some(mtm) = MainThreadMarker::new() else { return ExitCode::FAILURE };
        let args = Args::read();
        let app = child::application(mtm);
        let begin = match args.get("begin") {
            Some("down") => Begin::Down,
            Some("none") => Begin::Never,
            _ => Begin::Dragged,
        };
        let window = child::window(
            mtm,
            args.at(),
            &NSColor::grayColor(),
            args.get("level").unwrap_or("popup"),
            args.on("titled"),
        );
        let bounds = window.contentView().map(|v| v.bounds()).unwrap_or_default();
        let view = SourceView::new(
            mtm,
            bounds,
            Ivars {
                begin,
                first_mouse: args.get("first-mouse") != Some("0"),
                files: args.all("file").to_vec(),
                promises: args
                    .all("promise")
                    .iter()
                    .filter_map(|p| PromiseWriter::new(p))
                    .collect(),
                texts: args.all("text").to_vec(),
                magenta: args.get("image") != Some("clear"),
                mask: if args.get("mask") == Some("all") {
                    NSDragOperation::Copy
                        | NSDragOperation::Link
                        | NSDragOperation::Generic
                        | NSDragOperation::Move
                } else {
                    NSDragOperation::Copy
                },
                ignore: args.on("ignore"),
                begun: Cell::new(false),
                last: Cell::new((i64::MIN, i64::MIN)),
            },
        );
        window.setContentView(Some(&view));
        let _monitor = child::log_presses();
        // The first read of the cursor in a process is slow (AppKit's connection to the window
        // server), so it is paid before the parent starts posting.
        let warmed = slopty_capture::warm_cursor();
        let _timer = watch_cursor();
        child::ready(&window, &format!(" warm_ms={}", warmed.as_millis()));
        app.run();
        ExitCode::SUCCESS
    }
}

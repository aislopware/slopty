//! A drop target for the drag-and-drop spikes (`tests/spikes.rs`), which is its parent: the one
//! application those spikes drop on.
//!
//! It opens one borderless window, white, at `--at x,y,w,h` (global points from the main
//! display's top left), whose view takes file URLs, URLs and text, and answers every drag with
//! `--answer copy|link|none`. With `--spring 1` the view is spring loaded, as a Finder folder is.
//! It writes `ready pid=<pid> window=<CGWindowID>`, then one line for each callback AppKit makes:
//! `entered`, `updated` (when the point moves), `exited`, `prepare`, `perform` with what the drag
//! pasteboard holds, `file` for each file URL with whether it exists and its size, `conclude`,
//! `ended`, the `spring` callbacks, and `later`, what the same pasteboard holds half a second
//! after the drop (where a destination that loads its items afterwards reads them). It never
//! becomes the active application and leaves when its stdin closes.

#[cfg(target_os = "macos")]
#[path = "child.rs"]
#[expect(dead_code, reason = "shared with the drag source, which reads every value of a key")]
mod child;

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::Cell;
    use std::process::ExitCode;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2::{DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSColor, NSDragOperation, NSDraggingDestination, NSDraggingInfo, NSPasteboard,
        NSPasteboardTypeFileURL, NSPasteboardTypeString, NSPasteboardTypeURL, NSResponder,
        NSSpringLoadingDestination, NSSpringLoadingOptions, NSView,
    };
    use objc2_foundation::{NSArray, NSObject, NSRect, NSString, NSTimer, NSURL};

    use crate::child::{self, Args, say};

    pub struct Ivars {
        answer: NSDragOperation,
        spring: bool,
        /// Where the last `updated` line put the drag, so a still drag says nothing more.
        last: Cell<(i64, i64)>,
    }

    define_class!(
        // SAFETY:
        // - `NSView` may be subclassed; the overrides keep its contracts (a dragging and a
        //   spring-loading destination).
        // - `DropView` does not implement `Drop`.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SloptySpikeDropView"]
        #[ivars = Ivars]
        struct DropView;

        unsafe impl NSObjectProtocol for DropView {}

        unsafe impl NSDraggingDestination for DropView {
            #[unsafe(method(draggingEntered:))]
            fn dragging_entered(
                &self,
                info: &ProtocolObject<dyn NSDraggingInfo>,
            ) -> NSDragOperation {
                let at = info.draggingLocation();
                say(&format!(
                    "entered x={:.0} y={:.0} mask={} sequence={} types={}",
                    at.x,
                    at.y,
                    info.draggingSourceOperationMask().0,
                    info.draggingSequenceNumber(),
                    types(&info.draggingPasteboard()),
                ));
                self.ivars().answer
            }

            #[unsafe(method(draggingUpdated:))]
            fn dragging_updated(
                &self,
                info: &ProtocolObject<dyn NSDraggingInfo>,
            ) -> NSDragOperation {
                let at = info.draggingLocation();
                #[expect(clippy::cast_possible_truncation, reason = "a window's points")]
                let point = (at.x.round() as i64, at.y.round() as i64);
                if self.ivars().last.replace(point) != point {
                    say(&format!("updated x={} y={}", point.0, point.1));
                }
                self.ivars().answer
            }

            #[unsafe(method(draggingExited:))]
            fn dragging_exited(&self, _info: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
                say("exited");
            }

            #[unsafe(method(prepareForDragOperation:))]
            fn prepare(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
                say("prepare");
                self.ivars().answer != NSDragOperation::None
            }

            #[unsafe(method(performDragOperation:))]
            fn perform(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
                let pasteboard = info.draggingPasteboard();
                say(&format!("perform {}", contents(&pasteboard)));
                for path in file_paths(&pasteboard) {
                    let size = std::fs::metadata(&path).map(|m| m.len());
                    say(&format!(
                        "file path={path} exists={} size={}",
                        u8::from(size.is_ok()),
                        size.unwrap_or(0)
                    ));
                }
                let later = RcBlock::new(move |_timer| {
                    say(&format!("later {}", contents(&pasteboard)));
                });
                // SAFETY: AppKit rule: a one-shot timer on this, the main, run loop; the block
                // holds the drag pasteboard, which AppKit objects on the main thread may.
                let _timer = unsafe {
                    NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.5, false, &later)
                };
                true
            }

            #[unsafe(method(concludeDragOperation:))]
            fn conclude(&self, _info: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
                say("conclude");
            }

            #[unsafe(method(draggingEnded:))]
            fn dragging_ended(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) {
                say("ended");
            }
        }

        unsafe impl NSSpringLoadingDestination for DropView {
            #[unsafe(method(springLoadingActivated:draggingInfo:))]
            fn spring_activated(
                &self,
                activated: bool,
                _info: &ProtocolObject<dyn NSDraggingInfo>,
            ) {
                say(&format!("spring activated={}", u8::from(activated)));
            }

            #[unsafe(method(springLoadingHighlightChanged:))]
            fn spring_highlight(&self, info: &ProtocolObject<dyn NSDraggingInfo>) {
                say(&format!("spring highlight={}", info.springLoadingHighlight().0));
            }

            #[unsafe(method(springLoadingEntered:))]
            fn spring_entered(
                &self,
                _info: &ProtocolObject<dyn NSDraggingInfo>,
            ) -> NSSpringLoadingOptions {
                say("spring entered");
                self.spring_options()
            }

            #[unsafe(method(springLoadingUpdated:))]
            fn spring_updated(
                &self,
                _info: &ProtocolObject<dyn NSDraggingInfo>,
            ) -> NSSpringLoadingOptions {
                self.spring_options()
            }

            #[unsafe(method(springLoadingExited:))]
            fn spring_exited(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) {
                say("spring exited");
            }
        }
    );

    impl DropView {
        fn new(mtm: MainThreadMarker, frame: NSRect, ivars: Ivars) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(ivars);
            // SAFETY: `NSView`'s designated initialiser on a freshly allocated instance.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        fn spring_options(&self) -> NSSpringLoadingOptions {
            if self.ivars().spring {
                NSSpringLoadingOptions::Enabled
            } else {
                NSSpringLoadingOptions::Disabled
            }
        }
    }

    impl std::fmt::Debug for DropView {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("DropView")
        }
    }

    /// Every type on the pasteboard, joined by `,`.
    fn types(pasteboard: &NSPasteboard) -> String {
        pasteboard
            .types()
            .map(|types| types.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(","))
            .unwrap_or_default()
    }

    /// The file paths the pasteboard's items name.
    fn file_paths(pasteboard: &NSPasteboard) -> Vec<String> {
        let Some(items) = pasteboard.pasteboardItems() else { return Vec::new() };
        items
            .iter()
            // SAFETY: framework-provided constant string.
            .filter_map(|item| item.stringForType(unsafe { NSPasteboardTypeFileURL }))
            .filter_map(|url| NSURL::URLWithString(&url))
            .filter_map(|url| url.path().map(|p| p.to_string()))
            .collect()
    }

    /// What a drop holds: `items=<n> urls=<a|b> texts=<a|b>`.
    fn contents(pasteboard: &NSPasteboard) -> String {
        let items = pasteboard.pasteboardItems().map_or(0, |items| items.count());
        let read = |kind: &NSString| -> String {
            pasteboard
                .pasteboardItems()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.stringForType(kind))
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                        .join("|")
                })
                .unwrap_or_default()
        };
        // SAFETY: framework-provided constant strings.
        let (files, urls, texts) = unsafe {
            (read(NSPasteboardTypeFileURL), read(NSPasteboardTypeURL), read(NSPasteboardTypeString))
        };
        format!("items={items} files={files} urls={urls} texts={texts} types={}", types(pasteboard))
    }

    pub fn run() -> ExitCode {
        let Some(mtm) = MainThreadMarker::new() else { return ExitCode::FAILURE };
        let args = Args::read();
        let app = child::application(mtm);
        let answer = match args.get("answer") {
            Some("none") => NSDragOperation::None,
            Some("link") => NSDragOperation::Link,
            _ => NSDragOperation::Copy,
        };
        let window = child::window(mtm, args.at(), &NSColor::whiteColor(), "floating", false);
        let bounds = window.contentView().map(|v| v.bounds()).unwrap_or_default();
        let view = DropView::new(
            mtm,
            bounds,
            Ivars { answer, spring: args.on("spring"), last: Cell::new((i64::MIN, i64::MIN)) },
        );
        // SAFETY: framework-provided constant strings.
        let types = unsafe {
            NSArray::from_slice(&[
                NSPasteboardTypeFileURL,
                NSPasteboardTypeURL,
                NSPasteboardTypeString,
            ])
        };
        view.registerForDraggedTypes(&types);
        window.setContentView(Some(&view));
        let _monitor = child::log_presses();
        child::ready(&window, &format!(" answer={}", answer.0));
        app.run();
        ExitCode::SUCCESS
    }
}

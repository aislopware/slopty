//! An application that says what AppKit makes of the events posted to it, for the live gesture
//! test in `tests/inject.rs`: its parent, and the only process that posts to it.
//!
//! It opens one borderless window that nobody at the Mac sees and that catches none of their
//! pointer (clear, shadowless, at the desktop's level under every window), on the main display
//! because AppKit's swipe tracking cancels a gesture whose point is on none, and it never takes
//! the keyboard (an accessory app, ordered in with `orderFrontRegardless`). It writes `ready
//! pid=<pid> window=<CGWindowID> swipe_tracking=<0|1>` on stdout, then one line for each event it
//! takes off its queue, before AppKit dispatches it (`app type=<NSEventType> …`, with the fields
//! AppKit defines for that type; a press, a drag or a release is read and not dispatched, so
//! nothing raises the app). A sideways trackpad scroll is followed as Safari follows a swipe
//! between pages (`trackSwipeEventWithOptions:…`), one `track …` line for each step AppKit reports.
//! It leaves when its stdin closes, so it never outlives the test.
//!
//! With `--front`, for a guest of the VM lane only, it is instead an ordinary application: a
//! regular one whose titled window is ordered in on top at the normal level, at `--at <x>,<y>`
//! (AppKit's screen points, from the bottom left) or else at 200, 300. It never asks to be
//! activated itself: the test makes it active, as a person would, when it needs it so. Every
//! event is dispatched, so a drag by the title bar moves the window (`moved x=<x> y=<y>`), and
//! the window's view says what reaches it (`view <kind> …`, with the event's window and its
//! point in the window; `view first_mouse` for the click-through question, answered NO). Each
//! `app …` line then names the event's window and its point too, and a change in whether the app
//! is active or its window key says `state active=<0|1> key=<0|1>`. A second one at the same
//! place covers the first.
//!
//! A pointer event posted to a process reaches a view only bound to its window
//! (`slopty_input::backend::window_binding`). Without `--front` nothing is dispatched to a view
//! anyway: what is read here is what reaches the application, the events AppKit made.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::Cell;
    use std::io::Write as _;
    use std::process::ExitCode;
    use std::ptr::NonNull;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, NSObjectProtocol};
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEvent,
        NSEventMask, NSEventPhase, NSEventSwipeTrackingOptions, NSEventType, NSResponder, NSView,
        NSWindow, NSWindowStyleMask,
    };
    use objc2_core_graphics::{CGWindowLevelForKey, CGWindowLevelKey};
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize};

    /// On the main display, which AppKit's swipe tracking needs a gesture's point to be on.
    const ORIGIN: NSPoint = NSPoint { x: 0.0, y: 0.0 };
    /// Where `--front` puts its titled window unless told, clear of the menu bar and the Dock.
    const FRONT: NSPoint = NSPoint { x: 200.0, y: 300.0 };
    const SIZE: NSSize = NSSize { width: 400.0, height: 300.0 };

    /// One line on stdout, flushed: the parent reads them as they come.
    fn say(line: &str) {
        let mut out = std::io::stdout().lock();
        let _written = writeln!(out, "{line}").and_then(|()| out.flush());
    }

    define_class!(
        // SAFETY:
        // - `NSView` may be subclassed; the overrides only report the event and keep its
        //   contracts (AppKit, `NSResponder` event methods).
        // - `Seen` does not implement `Drop`.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SloptyInputTestView"]
        struct Seen;

        impl Seen {
            #[unsafe(method(acceptsFirstMouse:))]
            fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
                say("view first_mouse");
                false
            }

            #[unsafe(method(mouseDown:))]
            fn mouse_down(&self, event: &NSEvent) {
                say(&format!("view down {}", placed(event)));
            }

            #[unsafe(method(mouseDragged:))]
            fn mouse_dragged(&self, event: &NSEvent) {
                say(&format!("view dragged {}", placed(event)));
            }

            #[unsafe(method(mouseUp:))]
            fn mouse_up(&self, event: &NSEvent) {
                say(&format!("view up {}", placed(event)));
            }

            #[unsafe(method(scrollWheel:))]
            fn scroll_wheel(&self, event: &NSEvent) {
                say(&format!("view scroll dy={} {}", event.scrollingDeltaY(), placed(event)));
            }

            #[unsafe(method(magnifyWithEvent:))]
            fn magnify(&self, event: &NSEvent) {
                say(&format!("view magnify amount={} {}", event.magnification(), placed(event)));
            }
        }

        unsafe impl NSObjectProtocol for Seen {}
    );

    impl Seen {
        fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            // SAFETY: `NSView`'s designated initialiser on a freshly allocated instance.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }
    }

    /// A mouse event's press number, its window and its point in that window.
    fn placed(event: &NSEvent) -> String {
        let at = event.locationInWindow();
        let number = match event.r#type() {
            kind if is_press(kind) => format!("number={} ", event.eventNumber()),
            _ => String::new(),
        };
        format!("{number}window={} x={:.0} y={:.0}", event.windowNumber(), at.x, at.y)
    }

    /// `event`'s type and the fields AppKit defines for it; reading one it does not define for
    /// the type raises, so each type reads only its own.
    fn describe(event: &NSEvent) -> String {
        let kind = event.r#type();
        let phase = || event.phase().0;
        let fields = match kind {
            NSEventType::ScrollWheel => format!(
                " phase={} momentum={} dx={} dy={} precise={}",
                phase(),
                event.momentumPhase().0,
                event.scrollingDeltaX(),
                event.scrollingDeltaY(),
                u8::from(event.hasPreciseScrollingDeltas()),
            ),
            NSEventType::Magnify => format!(" phase={} amount={}", phase(), event.magnification()),
            NSEventType::Rotate => format!(" phase={} amount={}", phase(), event.rotation()),
            NSEventType::Swipe => format!(" dx={} dy={}", event.deltaX(), event.deltaY()),
            NSEventType::Gesture => format!(" phase={}", phase()),
            _ if is_press(kind) => {
                format!(" number={} button={}", event.eventNumber(), event.buttonNumber())
            }
            _ => String::new(),
        };
        format!("type={}{fields}", kind.0)
    }

    /// A press, a drag or a release of any button.
    fn is_press(kind: NSEventType) -> bool {
        [
            NSEventType::LeftMouseDown,
            NSEventType::LeftMouseDragged,
            NSEventType::LeftMouseUp,
            NSEventType::RightMouseDown,
            NSEventType::RightMouseDragged,
            NSEventType::RightMouseUp,
            NSEventType::OtherMouseDown,
            NSEventType::OtherMouseDragged,
            NSEventType::OtherMouseUp,
        ]
        .contains(&kind)
    }

    /// Follow a sideways trackpad scroll as Safari follows a swipe between pages: from the
    /// first scroll of a gesture that moves more across than down, once per gesture, saying each
    /// step AppKit's tracking reports. `tracking` is whether this gesture was taken.
    fn track_swipe(event: &NSEvent, tracking: &Cell<bool>) {
        if event.r#type() != NSEventType::ScrollWheel || !event.hasPreciseScrollingDeltas() {
            return;
        }
        let phase = event.phase();
        if phase == NSEventPhase::None {
            return;
        }
        if phase.intersects(NSEventPhase::Ended | NSEventPhase::Cancelled) {
            tracking.set(false);
            return;
        }
        if tracking.get() || event.scrollingDeltaX().abs() <= event.scrollingDeltaY().abs() {
            return;
        }
        tracking.set(true);
        let handler =
            RcBlock::new(|amount: f64, phase: NSEventPhase, done: Bool, _stop: NonNull<Bool>| {
                let done = u8::from(done.as_bool());
                say(&format!("track amount={amount:.3} phase={} complete={done}", phase.0));
            });
        event.trackSwipeEventWithOptions_dampenAmountThresholdMin_max_usingHandler(
            NSEventSwipeTrackingOptions::empty(),
            -1.0,
            1.0,
            &handler,
        );
    }

    pub fn run() -> ExitCode {
        let Some(mtm) = MainThreadMarker::new() else { return ExitCode::FAILURE };
        let app = NSApplication::sharedApplication(mtm);
        let args: Vec<String> = std::env::args().collect();
        let front = args.iter().any(|arg| arg == "--front");
        let policy = if front {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        };
        app.setActivationPolicy(policy);
        app.finishLaunching();

        let (origin, style) = if front {
            let at = args
                .iter()
                .position(|arg| arg == "--at")
                .and_then(|i| args.get(i.checked_add(1)?)?.split_once(','))
                .and_then(|(x, y)| Some(NSPoint { x: x.parse().ok()?, y: y.parse().ok()? }));
            (at.unwrap_or(FRONT), NSWindowStyleMask::Titled)
        } else {
            (ORIGIN, NSWindowStyleMask::Borderless)
        };
        // SAFETY: the designated `NSWindow` initialiser; every argument is a plain value and the
        // window is created and used on the main thread (AppKit, `NSWindow`).
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect { origin, size: SIZE },
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: `false` stops AppKit freeing a window this code still holds (AppKit,
        // `NSWindow.isReleasedWhenClosed`).
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        // Nothing to see, and nothing in the way: clear and shadowless, so the window server
        // passes every real click through it, at the desktop's level, under every window.
        if front {
            window.setContentView(Some(&Seen::new(
                mtm,
                NSRect { origin: NSPoint::ZERO, size: SIZE },
            )));
            window.makeKeyAndOrderFront(None);
        } else {
            window.setOpaque(false);
            window.setBackgroundColor(Some(&NSColor::clearColor()));
            window.setHasShadow(false);
            let desktop = CGWindowLevelForKey(CGWindowLevelKey::DesktopWindowLevelKey);
            window.setLevel(desktop as isize);
            window.orderFrontRegardless();
        }
        say(&format!(
            "ready pid={} window={} swipe_tracking={}",
            std::process::id(),
            window.windowNumber(),
            u8::from(NSEvent::isSwipeTrackingFromScrollEventsEnabled()),
        ));

        // The parent closing stdin, or going, ends this process.
        let gone = Arc::new(AtomicBool::new(false));
        std::thread::spawn({
            let gone = Arc::clone(&gone);
            move || {
                let _read = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
                gone.store(true, Ordering::Relaxed);
            }
        });
        let tracking = Cell::new(false);
        let mut at = window.frame().origin;
        let mut state = (false, false);
        while !gone.load(Ordering::Relaxed) {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.05);
            // SAFETY: the main thread's own queue, read in the default mode as `-[NSApplication
            // run]` does (AppKit, `nextEventMatchingMask:untilDate:inMode:dequeue:`).
            let event = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&until),
                    NSDefaultRunLoopMode,
                    true,
                )
            };
            if let Some(event) = event {
                let placed = match event.r#type() {
                    NSEventType::ScrollWheel | NSEventType::Magnify => true,
                    kind => is_press(kind),
                };
                if front && placed {
                    say(&format!("app {} {}", describe(&event), self::placed(&event)));
                } else {
                    say(&format!("app {}", describe(&event)));
                }
                track_swipe(&event, &tracking);
                // A press is read, not acted on: nothing here raises the app.
                if front || !is_press(event.r#type()) {
                    app.sendEvent(&event);
                }
            }
            let now = window.frame().origin;
            if now != at {
                at = now;
                say(&format!("moved x={} y={}", now.x, now.y));
            }
            let is = (app.isActive(), window.isKeyWindow());
            if front && is != state {
                state = is;
                say(&format!("state active={} key={}", u8::from(is.0), u8::from(is.1)));
            }
        }
        ExitCode::SUCCESS
    }
}

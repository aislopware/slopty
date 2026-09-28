//! System shortcuts for a remote Mac, taken off this one while a remote tile has the keyboard.
//!
//! These are the chords macOS acts on before any app sees them (⌘Tab, ⌘Space, ⌃← and ⌃→
//! between Spaces, ⌃↑ Mission Control, ⌘⇧3/4/5); the app sends them to the worker instead
//! (`docs/decisions/input.md`, "System shortcuts go to the remote Mac through a session tap").
//!
//! The chord filter ([`chord`]) is pure. The tap ([`Tap`]) is a `CGEventTap` on the session,
//! which macOS allows an app only with Accessibility: it is made only when the person turns the
//! toggle on, never by a test.

use slopty_proto::input::{KeyCode, Mods};

/// A system chord taken off this Mac: the key, pressed or let go, with the modifiers held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chord {
    /// The key.
    pub code: KeyCode,
    /// Pressed (and its repeats), or let go.
    pub down: bool,
    /// The modifiers held with it.
    pub mods: Mods,
}

// Virtual key codes from Carbon's `<HIToolbox/Events.h>`, which objc2 does not bind.
/// `kVK_Tab`.
const VK_TAB: u16 = 0x30;
/// `kVK_Space`.
const VK_SPACE: u16 = 0x31;
/// `kVK_ANSI_3`.
const VK_3: u16 = 0x14;
/// `kVK_ANSI_4`.
const VK_4: u16 = 0x15;
/// `kVK_ANSI_5`.
const VK_5: u16 = 0x17;
/// `kVK_LeftArrow`.
const VK_LEFT: u16 = 0x7b;
/// `kVK_RightArrow`.
const VK_RIGHT: u16 = 0x7c;
/// `kVK_DownArrow`.
const VK_DOWN: u16 = 0x7d;
/// `kVK_UpArrow`.
const VK_UP: u16 = 0x7e;

/// The key of a chord macOS takes before an app sees it: virtual code `keycode` with `mods`.
///
/// `None` for every other chord, which reaches the app and goes to the worker its usual way.
///
/// Taken: ⌘Tab and ⌘⇧Tab (the app switcher), ⌘Space, ⌃Space and ⌥⌘Space (Spotlight, the input
/// source, Finder search), ⌃ with an arrow (Spaces, Mission Control, the app's windows) and
/// ⌘⇧3, ⌘⇧4 and ⌘⇧5 (screenshots). Left on this Mac: ⌃Tab, the keyboard's way out of the
/// tile; ⌘⌥Esc, force quit, the way out of anything; ⌃⌘Q, which locks the Mac in front of you.
#[must_use]
pub const fn chord(keycode: u16, mods: Mods) -> Option<KeyCode> {
    let cmd = mods.contains(Mods::SUPER);
    let ctrl = mods.contains(Mods::CTRL);
    let shift = mods.contains(Mods::SHIFT);
    match keycode {
        VK_TAB if cmd => Some(KeyCode::Tab),
        VK_SPACE if cmd || ctrl => Some(KeyCode::Space),
        VK_LEFT if ctrl && !cmd => Some(KeyCode::ArrowLeft),
        VK_RIGHT if ctrl && !cmd => Some(KeyCode::ArrowRight),
        VK_UP if ctrl && !cmd => Some(KeyCode::ArrowUp),
        VK_DOWN if ctrl && !cmd => Some(KeyCode::ArrowDown),
        VK_3 if cmd && shift => Some(KeyCode::Digit3),
        VK_4 if cmd && shift => Some(KeyCode::Digit4),
        VK_5 if cmd && shift => Some(KeyCode::Digit5),
        _ => None,
    }
}

/// What a chord filter decides about one key event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// It goes on to macOS and the app.
    Pass,
    /// It is taken off this Mac and sent to the worker.
    Take(Chord),
}

/// The tap's memory between events: the keys whose press it took.
///
/// Their release is taken too, even once the tile has let the keyboard go: macOS would
/// otherwise see a release it never saw pressed, and the worker keep a key down.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    held: Vec<u16>,
}

impl Filter {
    /// Decide a key press (`down`) or release of `keycode` with `mods`, `armed` while a remote
    /// tile has the keyboard.
    pub fn key(&mut self, keycode: u16, down: bool, mods: Mods, armed: bool) -> Verdict {
        let held = self.held.contains(&keycode);
        if !down {
            if !held {
                return Verdict::Pass;
            }
            self.held.retain(|k| *k != keycode);
            return chord_of(keycode, mods).map_or(Verdict::Pass, Verdict::Take);
        }
        if !armed && !held {
            return Verdict::Pass;
        }
        let Some(code) = chord(keycode, mods).or_else(|| held.then(|| key_of(keycode)).flatten())
        else {
            return Verdict::Pass;
        };
        if !held {
            self.held.push(keycode);
        }
        Verdict::Take(Chord { code, down: true, mods })
    }
}

/// The release of a key whose press was taken, whatever is held now.
fn chord_of(keycode: u16, mods: Mods) -> Option<Chord> {
    key_of(keycode).map(|code| Chord { code, down: false, mods })
}

/// The key a system chord's virtual code is.
const fn key_of(keycode: u16) -> Option<KeyCode> {
    Some(match keycode {
        VK_TAB => KeyCode::Tab,
        VK_SPACE => KeyCode::Space,
        VK_LEFT => KeyCode::ArrowLeft,
        VK_RIGHT => KeyCode::ArrowRight,
        VK_UP => KeyCode::ArrowUp,
        VK_DOWN => KeyCode::ArrowDown,
        VK_3 => KeyCode::Digit3,
        VK_4 => KeyCode::Digit4,
        VK_5 => KeyCode::Digit5,
        _ => return None,
    })
}

#[cfg(target_os = "macos")]
pub use tap::{Tap, TapError, granted, request};

#[cfg(target_os = "macos")]
mod tap {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource};
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventTapProxy, CGEventType, CGPreflightPostEventAccess,
        CGRequestPostEventAccess,
    };
    use slopty_proto::input::Mods;
    use tokio::sync::mpsc::UnboundedSender;

    use super::{Chord, Filter, Verdict};

    /// Why a tap could not be made.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum TapError {
        /// Not on the main thread, whose run loop the tap runs on.
        NotMain,
        /// macOS refused the tap: Slopty is not allowed Accessibility.
        NotAllowed,
    }

    impl std::fmt::Display for TapError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(match self {
                Self::NotMain => "the system shortcut tap is made on the main thread",
                Self::NotAllowed => "Accessibility is not allowed",
            })
        }
    }

    impl std::error::Error for TapError {}

    /// Whether this app may take keys off the session: Accessibility is allowed. Asks nothing.
    #[must_use]
    pub fn granted() -> bool {
        CGPreflightPostEventAccess()
    }

    /// Ask for Accessibility: macOS shows its prompt once, and System Settings after.
    pub fn request() -> bool {
        CGRequestPostEventAccess()
    }

    /// What the callback reads: the filter, whether to take chords now, where they go, and the
    /// tap itself, to turn back on when macOS turns it off.
    struct State {
        filter: RefCell<Filter>,
        armed: Arc<AtomicBool>,
        chords: UnboundedSender<Chord>,
        port: RefCell<Option<CFRetained<CFMachPort>>>,
    }

    /// A session event tap that takes system chords while armed; it goes when dropped. Made,
    /// used and dropped on the main thread.
    #[derive(Debug)]
    pub struct Tap {
        port: CFRetained<CFMachPort>,
        source: CFRetained<CFRunLoopSource>,
        run_loop: CFRetained<CFRunLoop>,
        state: NonNull<c_void>,
        armed: Arc<AtomicBool>,
    }

    impl Tap {
        /// Tap the session's key events on the main run loop, sending the chords it takes to
        /// `chords`. It takes none until [`Self::arm`].
        ///
        /// # Errors
        ///
        /// [`TapError::NotAllowed`] without Accessibility (see [`request`]).
        pub fn install(chords: UnboundedSender<Chord>) -> Result<Self, TapError> {
            if objc2::MainThreadMarker::new().is_none() {
                return Err(TapError::NotMain);
            }
            let armed = Arc::new(AtomicBool::new(false));
            let state = Box::new(State {
                filter: RefCell::new(Filter::default()),
                armed: Arc::clone(&armed),
                chords,
                port: RefCell::new(None),
            });
            let state = NonNull::from(Box::leak(state)).cast::<c_void>();
            // The bits of `CGEventType::KeyDown` (10) and `KeyUp` (11).
            let mask: CGEventMask = 0b1100_0000_0000;
            // SAFETY: `callback` matches `CGEventTapCallBack`, and `state` is a live `State`
            // that outlives the tap: `Drop` invalidates the port before it frees it.
            let port = unsafe {
                CGEvent::tap_create(
                    CGEventTapLocation::SessionEventTap,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::Default,
                    mask,
                    Some(callback),
                    state.as_ptr(),
                )
            };
            let Some(port) = port else {
                // SAFETY: `state` came from `Box::leak` above and no tap holds it.
                drop(unsafe { Box::from_raw(state.cast::<State>().as_ptr()) });
                return Err(TapError::NotAllowed);
            };
            let (Some(source), Some(run_loop)) =
                (CFMachPort::new_run_loop_source(None, Some(&port), 0), CFRunLoop::main())
            else {
                port.invalidate();
                // SAFETY: the port is invalidated, so no callback holds `state`.
                drop(unsafe { Box::from_raw(state.cast::<State>().as_ptr()) });
                return Err(TapError::NotAllowed);
            };
            // SAFETY: a `CFRunLoopMode` static CoreFoundation defines, read-only.
            let common = unsafe { objc2_core_foundation::kCFRunLoopCommonModes };
            run_loop.add_source(Some(&source), common);
            // SAFETY: `state` is the live `State` leaked above; only the main thread (this
            // one, and the callback's) touches it.
            unsafe { state.cast::<State>().as_ref() }.port.replace(Some(CFRetained::clone(&port)));
            Ok(Self { port, source, run_loop, state, armed })
        }

        /// Take system chords (a remote tile has the keyboard), or let them be.
        pub fn arm(&self, on: bool) {
            self.armed.store(on, Ordering::Relaxed);
        }
    }

    impl Drop for Tap {
        fn drop(&mut self) {
            self.port.invalidate();
            // SAFETY: a `CFRunLoopMode` static CoreFoundation defines, read-only.
            let common = unsafe { objc2_core_foundation::kCFRunLoopCommonModes };
            self.run_loop.remove_source(Some(&self.source), common);
            // SAFETY: `CFMachPortInvalidate` stops the callbacks, so nothing reads `state` now;
            // it came from `Box::leak` in `install`.
            let state = unsafe { Box::from_raw(self.state.cast::<State>().as_ptr()) };
            state.port.replace(None);
        }
    }

    /// The modifiers of a key event.
    fn mods(flags: CGEventFlags) -> Mods {
        [
            (CGEventFlags::MaskCommand, Mods::SUPER),
            (CGEventFlags::MaskControl, Mods::CTRL),
            (CGEventFlags::MaskAlternate, Mods::ALT),
            (CGEventFlags::MaskShift, Mods::SHIFT),
        ]
        .into_iter()
        .filter(|(flag, _)| flags.contains(*flag))
        .fold(Mods::empty(), |all, (_, m)| all | m)
    }

    /// The tap's callback: a taken event is sent on and swallowed (null returned); every other
    /// goes on. A tap macOS turned off for being slow or by the person is turned back on.
    unsafe extern "C-unwind" fn callback(
        _proxy: CGEventTapProxy,
        kind: CGEventType,
        event: NonNull<CGEvent>,
        info: *mut c_void,
    ) -> *mut CGEvent {
        // SAFETY: `info` is the `State` `Tap::install` leaked, alive until the port is
        // invalidated, and CoreGraphics calls back on the main run loop only.
        let Some(state) = (unsafe { info.cast::<State>().as_ref() }) else {
            return event.as_ptr();
        };
        if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput
        {
            if let Some(port) = state.port.borrow().as_ref() {
                CGEvent::tap_enable(port, true);
            }
            return event.as_ptr();
        }
        let down = kind == CGEventType::KeyDown;
        if !down && kind != CGEventType::KeyUp {
            return event.as_ptr();
        }
        // SAFETY: CoreGraphics hands the callback a live event.
        let event_ref = unsafe { event.as_ref() };
        let keycode =
            CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode);
        let Ok(keycode) = u16::try_from(keycode) else { return event.as_ptr() };
        let mods = mods(CGEvent::flags(Some(event_ref)));
        // Armed only while the app is frontmost, whatever the app last said: a key typed into
        // another app is never taken.
        let armed = state.armed.load(Ordering::Relaxed)
            && objc2_app_kit::NSRunningApplication::currentApplication().isActive();
        match state.filter.borrow_mut().key(keycode, down, mods, armed) {
            Verdict::Pass => event.as_ptr(),
            Verdict::Take(chord) => {
                let _sent = state.chords.send(chord);
                std::ptr::null_mut()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::input::{KeyCode, Mods};

    use super::{Chord, Filter, VK_3, VK_LEFT, VK_SPACE, VK_TAB, Verdict, chord};

    /// The system's chords are taken and the ways out are not: ⌘Tab, ⌘Space, ⌃Space, ⌃←, ⌘⇧3
    /// are; ⌃Tab, a plain Tab or Space, ⌃⌘←, ⌘3 and every other key are left alone.
    #[test]
    fn the_filter_takes_the_system_chords_only() {
        let cmd = Mods::SUPER;
        assert_eq!(chord(VK_TAB, cmd), Some(KeyCode::Tab));
        assert_eq!(chord(VK_TAB, cmd | Mods::SHIFT), Some(KeyCode::Tab), "backwards too");
        assert_eq!(chord(VK_TAB, Mods::CTRL), None, "⌃Tab is the way out of the tile");
        assert_eq!(chord(VK_TAB, Mods::empty()), None);
        assert_eq!(chord(VK_SPACE, cmd), Some(KeyCode::Space));
        assert_eq!(chord(VK_SPACE, Mods::CTRL), Some(KeyCode::Space), "the input source");
        assert_eq!(chord(VK_SPACE, Mods::SHIFT), None);
        assert_eq!(chord(VK_LEFT, Mods::CTRL), Some(KeyCode::ArrowLeft));
        assert_eq!(chord(VK_LEFT, Mods::CTRL | cmd), None);
        assert_eq!(chord(VK_3, cmd | Mods::SHIFT), Some(KeyCode::Digit3));
        assert_eq!(chord(VK_3, cmd), None);
        // `kVK_Escape` with ⌘⌥ (force quit) and `kVK_ANSI_Q` with ⌃⌘ (lock) stay here.
        assert_eq!(chord(0x35, cmd | Mods::ALT), None);
        assert_eq!(chord(0x0c, cmd | Mods::CTRL), None);
    }

    /// Nothing is taken until armed; a taken key's repeats and release are taken with it even
    /// once the tile has let the keyboard go and ⌘ is up, and a release never pressed passes.
    #[test]
    fn a_taken_press_takes_its_release() {
        let mut filter = Filter::default();
        let cmd = Mods::SUPER;
        assert_eq!(filter.key(VK_TAB, true, cmd, false), Verdict::Pass, "not armed");
        assert_eq!(filter.key(VK_TAB, false, cmd, false), Verdict::Pass);
        let press = Chord { code: KeyCode::Tab, down: true, mods: cmd };
        assert_eq!(filter.key(VK_TAB, true, cmd, true), Verdict::Take(press));
        assert_eq!(filter.key(VK_TAB, true, cmd, false), Verdict::Take(press), "its repeat");
        assert_eq!(
            filter.key(VK_TAB, false, Mods::empty(), false),
            Verdict::Take(Chord { code: KeyCode::Tab, down: false, mods: Mods::empty() })
        );
        assert_eq!(filter.key(VK_TAB, false, Mods::empty(), true), Verdict::Pass, "let go once");
        assert_eq!(filter.key(0x00, true, cmd, true), Verdict::Pass, "⌘A is the app's");
    }
}

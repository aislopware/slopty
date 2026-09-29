//! System shortcuts for a remote Mac, taken off this one while a remote tile has the keyboard.
//!
//! macOS acts on its symbolic hotkeys (⌘Tab, ⌘Space, ⌃← and ⌃→ between Spaces, ⌃↑ Mission
//! Control, ⌘⇧3/4/5, ⌃1…⌃9, the input-source keys, and every one the person customised) in the
//! WindowServer, before any app sees them. While a tile with system shortcuts on has the
//! keyboard and this app is frontmost, the hotkey layer is off ([`HotkeysOff`], `HIToolbox`'s
//! `PushSymbolicHotKeyMode`), so each arrives as an ordinary key and goes to the worker the way
//! every key does; Force Quit (⌘⌥⎋) and the Accessibility hotkeys stay on. The tap takes the
//! fixed list of chords ([`chord`]) as well, whether or not the layer went off: a pushed mode
//! is handed back even when the WindowServer does not apply it, so the list is the floor that
//! holds either way, and a chord it takes never reaches the app to go twice. The tap also takes
//! play/pause and the track keys, which are no keys at all but system-defined events; volume
//! and brightness stay here, where the stream's sound plays (`docs/decisions/input.md`,
//! "System shortcuts: the WindowServer's hotkey layer goes off").
//!
//! The chord filter ([`chord`]), the media keys ([`media`]) and the hotkey guard are pure or
//! behind a seam ([`HotkeyLayer`]). The tap ([`Tap`]) is a `CGEventTap` on the session, which
//! macOS allows an app only with Accessibility: it is made only when the person turns the
//! toggle on, never by a test.

use slopty_proto::input::{KeyCode, Mods};

use crate::keyboard::nx;

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

/// The media key a `data1` carries, when the worker takes it, and whether it went down.
///
/// `NX_SUBTYPE_AUX_CONTROL_BUTTONS` events carry the key in bits 16–31 and its state in bits
/// 8–15. `None` for the keys that stay on this Mac, volume and brightness above all.
#[must_use]
pub const fn media(data1: isize) -> Option<(KeyCode, bool)> {
    let down = (data1 >> 8) & 0xff == nx::KEYDOWN;
    let code = match (data1 >> 16) & 0xffff {
        nx::KEYTYPE_PLAY => KeyCode::MediaPlayPause,
        nx::KEYTYPE_NEXT | nx::KEYTYPE_FAST => KeyCode::MediaTrackNext,
        nx::KEYTYPE_PREVIOUS | nx::KEYTYPE_REWIND => KeyCode::MediaTrackPrevious,
        _ => return None,
    };
    Some((code, down))
}

/// The WindowServer's symbolic hotkey layer: on (macOS acts on ⌘Tab and the rest) or off
/// (they reach the app as keys). A seam, so the guard is tested without the WindowServer.
pub trait HotkeyLayer {
    /// What turning the layer off hands back, to turn it on again with.
    type Token;
    /// Turn the layer off; `None` when macOS would not.
    fn off(&self) -> Option<Self::Token>;
    /// Turn the layer back on.
    fn on(&self, token: Self::Token);
}

/// The hotkey layer, off for as long as this lives: turned back on when dropped, so a tap
/// dropped, disarmed or unwound by a panic leaves the person's shortcuts working.
pub struct HotkeysOff<L: HotkeyLayer> {
    layer: L,
    token: Option<L::Token>,
}

impl<L: HotkeyLayer> std::fmt::Debug for HotkeysOff<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HotkeysOff").finish_non_exhaustive()
    }
}

impl<L: HotkeyLayer> HotkeysOff<L> {
    /// Turn `layer` off; `None` when macOS would not, and the layer stays as it was.
    pub fn new(layer: L) -> Option<Self> {
        // Lazily: a guard built for a refusal would turn the layer on as it drops.
        let token = layer.off()?;
        Some(Self { layer, token: Some(token) })
    }
}

impl<L: HotkeyLayer> Drop for HotkeysOff<L> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.layer.on(token);
        }
    }
}

/// Which of the system's shortcuts go to the worker while armed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Taking {
    /// None: not armed, or this app is not frontmost.
    Off,
    /// Every one: the hotkey layer is off, so each reaches the app as a key.
    Every,
    /// Only the tap's fixed list ([`chord`]): the hotkey layer could not be turned off.
    Chords,
}

/// The hotkey layer, off exactly while the view wants the shortcuts and this app is frontmost.
///
/// Leaving the app turns it back on at once, whatever the view last said, so the person's ⌘Tab
/// works in every other app even before the view hears of it.
pub struct Hotkeys<L: HotkeyLayer + Clone> {
    /// The layer; `None` where this macOS lacks the calls.
    layer: Option<L>,
    wanted: bool,
    active: bool,
    off: Option<HotkeysOff<L>>,
}

impl<L: HotkeyLayer + Clone> std::fmt::Debug for Hotkeys<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hotkeys").field("taking", &self.taking()).finish_non_exhaustive()
    }
}

impl<L: HotkeyLayer + Clone> Hotkeys<L> {
    /// Not armed; `active` says whether this app is frontmost now.
    pub const fn new(layer: Option<L>, active: bool) -> Self {
        Self { layer, wanted: false, active, off: None }
    }

    /// The view wants the shortcuts (`on`), or lets them be: what goes now.
    pub fn arm(&mut self, on: bool) -> Taking {
        self.wanted = on;
        self.follow();
        self.taking()
    }

    /// This app became frontmost, or stopped being.
    pub fn app_active(&mut self, active: bool) {
        self.active = active;
        self.follow();
    }

    /// Which shortcuts go now.
    #[must_use]
    pub const fn taking(&self) -> Taking {
        if !self.wanted || !self.active {
            Taking::Off
        } else if self.off.is_some() {
            Taking::Every
        } else {
            Taking::Chords
        }
    }

    fn follow(&mut self) {
        if !(self.wanted && self.active) {
            self.off = None;
        } else if self.off.is_none() {
            self.off = self.layer.clone().and_then(HotkeysOff::new);
        }
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
pub use symbolic::{Symbolic, hotkeys_back_on};
#[cfg(target_os = "macos")]
pub use tap::{Tap, TapError, granted, request};

#[cfg(target_os = "macos")]
mod symbolic {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use super::HotkeyLayer;

    /// `kHIHotKeyModeAllDisabledExceptUniversalAccess` (`<HIToolbox/CarbonEvents.h>`): every
    /// symbolic hotkey off but the Accessibility ones. Force Quit (⌘⌥⎋) is among those the
    /// WindowServer keeps in this mode, so the way out of anything stays on this Mac.
    const ALL_DISABLED_EXCEPT_UNIVERSAL_ACCESS: u32 = 1 << 1;

    // `<HIToolbox/CarbonEvents.h>`, in Carbon; objc2 binds none of it. "Not thread safe": the
    // main thread only, as the tap is.
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn PushSymbolicHotKeyMode(options: u32) -> *mut c_void;
        fn PopSymbolicHotKeyMode(token: *mut c_void);
    }

    thread_local! {
        /// The modes this app pushed and has not popped, so the app's end pops them all.
        static PUSHED: RefCell<Vec<NonNull<c_void>>> = const { RefCell::new(Vec::new()) };
    }

    /// `HIToolbox`'s hotkey mode stack: `PushSymbolicHotKeyMode` and `PopSymbolicHotKeyMode`.
    ///
    /// A mode pushed while the app is frontmost holds only while it stays frontmost, and it
    /// reverts when the app exits without popping it (`CarbonEvents.h`), so neither a crash nor
    /// another app coming to the front leaves the person's shortcuts off. macOS changes the mode
    /// only for an app allowed Accessibility, which the tap needs anyway. Main thread only.
    #[derive(Clone, Copy, Debug)]
    pub struct Symbolic;

    impl HotkeyLayer for Symbolic {
        type Token = NonNull<c_void>;

        fn off(&self) -> Option<Self::Token> {
            // Without Accessibility the mode is stacked but never applied (`CarbonEvents.h`),
            // and the tap could not have been made.
            if objc2::MainThreadMarker::new().is_none() || !super::tap::granted() {
                return None;
            }
            // SAFETY: on the main thread; an `OptionBits` of the header's modes.
            let token = NonNull::new(unsafe {
                PushSymbolicHotKeyMode(ALL_DISABLED_EXCEPT_UNIVERSAL_ACCESS)
            })?;
            PUSHED.with(|pushed| pushed.borrow_mut().push(token));
            Some(token)
        }

        fn on(&self, token: Self::Token) {
            let pushed = PUSHED.with(|pushed| {
                let mut pushed = pushed.borrow_mut();
                let at = pushed.iter().position(|&t| t == token)?;
                Some(pushed.swap_remove(at))
            });
            if let Some(token) = pushed {
                // SAFETY: a token `PushSymbolicHotKeyMode` returned, popped once, on the main
                // thread it was pushed on.
                unsafe {
                    PopSymbolicHotKeyMode(token.as_ptr());
                }
            }
        }
    }

    /// Pop every mode this app pushed: as it terminates, where GPUI drops no view and no tap.
    /// Whether one was still pushed.
    pub fn hotkeys_back_on() -> bool {
        let pushed = PUSHED.with(|pushed| std::mem::take(&mut *pushed.borrow_mut()));
        for token in &pushed {
            // SAFETY: tokens `PushSymbolicHotKeyMode` returned on this (the main) thread, each
            // popped once: they left the list as they are popped.
            unsafe {
                PopSymbolicHotKeyMode(token.as_ptr());
            }
        }
        !pushed.is_empty()
    }
}

#[cfg(target_os = "macos")]
mod tap {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_app_kit::{
        NSApplication, NSApplicationDidBecomeActiveNotification,
        NSApplicationDidResignActiveNotification,
    };
    use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource};
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventTapProxy, CGEventType, CGPreflightPostEventAccess,
        CGRequestPostEventAccess,
    };
    use objc2_foundation::{NSNotification, NSNotificationCenter};
    use slopty_proto::input::Mods;
    use tokio::sync::mpsc::UnboundedSender;

    use super::{Chord, Filter, Hotkeys, Symbolic, Taking, Verdict, media, nx};

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

    /// What the callback reads: the filter, whether to take chords now, where they go, and
    /// the tap itself, to turn back on when macOS turns it off.
    struct State {
        filter: RefCell<Filter>,
        armed: Arc<AtomicBool>,
        chords: UnboundedSender<Chord>,
        port: RefCell<Option<CFRetained<CFMachPort>>>,
    }

    /// The hotkey layer's state, shared by the tap and its app-activation observers.
    type Layer = Rc<RefCell<Hotkeys<Symbolic>>>;

    /// A session event tap that takes system chords while armed; it goes when dropped.
    ///
    /// While armed and this app is frontmost, the WindowServer's hotkey layer is off too. Made,
    /// used and dropped on the main thread.
    #[derive(Debug)]
    pub struct Tap {
        port: CFRetained<CFMachPort>,
        source: CFRetained<CFRunLoopSource>,
        run_loop: CFRetained<CFRunLoop>,
        state: NonNull<c_void>,
        armed: Arc<AtomicBool>,
        hotkeys: Layer,
        /// `NSApplicationDidResignActiveNotification`'s and `…DidBecomeActive…`'s observers.
        observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    }

    impl Tap {
        /// Tap the session's key events on the main run loop, sending the chords it takes to
        /// `chords`. It takes none until [`Self::arm`].
        ///
        /// # Errors
        ///
        /// [`TapError::NotAllowed`] without Accessibility (see [`request`]).
        pub fn install(chords: UnboundedSender<Chord>) -> Result<Self, TapError> {
            let mtm = objc2::MainThreadMarker::new().ok_or(TapError::NotMain)?;
            let armed = Arc::new(AtomicBool::new(false));
            let active = NSApplication::sharedApplication(mtm).isActive();
            let hotkeys: Layer = Rc::new(RefCell::new(Hotkeys::new(Some(Symbolic), active)));
            let state = Box::new(State {
                filter: RefCell::new(Filter::default()),
                armed: Arc::clone(&armed),
                chords,
                port: RefCell::new(None),
            });
            let state = NonNull::from(Box::leak(state)).cast::<c_void>();
            // The bits of `CGEventType::KeyDown` (10), `KeyUp` (11) and `NX_SYSDEFINED` (14).
            let mask: CGEventMask = 0b0100_1100_0000_0000;
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
            let observers = observe_activation(&hotkeys);
            Ok(Self { port, source, run_loop, state, armed, hotkeys, observers })
        }

        /// Take system shortcuts (a remote tile has the keyboard): the hotkey layer goes off
        /// while this app is frontmost and the media keys are taken; or let them be, the layer
        /// back on. What goes now: every shortcut, or only the tap's list when the
        /// WindowServer kept its layer.
        pub fn arm(&self, on: bool) -> Taking {
            self.armed.store(on, Ordering::Relaxed);
            self.hotkeys.borrow_mut().arm(on)
        }
    }

    /// Follow this app in and out of the front: the layer goes back on the moment another app
    /// is frontmost, before any view hears of it, and off again on the way back while armed.
    fn observe_activation(hotkeys: &Layer) -> Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>> {
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: immutable `NSString` statics AppKit defines.
        let names = unsafe {
            [
                (NSApplicationDidResignActiveNotification, false),
                (NSApplicationDidBecomeActiveNotification, true),
            ]
        };
        names
            .into_iter()
            .map(|(name, active)| {
                let hotkeys = Rc::clone(hotkeys);
                let block = RcBlock::new(move |_note: NonNull<NSNotification>| {
                    hotkeys.borrow_mut().app_active(active);
                });
                // SAFETY: with no queue the block runs on the thread that posts, and AppKit
                // posts its application notifications on the main thread, the only one that
                // touches the `Rc`s the block holds; the tap removes the observer before they
                // go (`Drop`).
                unsafe {
                    center.addObserverForName_object_queue_usingBlock(
                        Some(name),
                        None,
                        None,
                        &block,
                    )
                }
            })
            .collect()
    }

    impl Drop for Tap {
        fn drop(&mut self) {
            let center = NSNotificationCenter::defaultCenter();
            for observer in self.observers.drain(..) {
                // SAFETY: an observer `addObserverForName:…` returned, removed once.
                unsafe {
                    center.removeObserver(observer.as_ref());
                }
            }
            self.hotkeys.borrow_mut().arm(false);
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
        // SAFETY: CoreGraphics hands the callback a live event.
        let event_ref = unsafe { event.as_ref() };
        // Armed only while the app is frontmost, whatever the app last said: a key typed into
        // another app is never taken.
        let armed = || {
            state.armed.load(Ordering::Relaxed)
                && objc2_app_kit::NSRunningApplication::currentApplication().isActive()
        };
        if kind.0 == nx::SYSDEFINED {
            if !armed() {
                return event.as_ptr();
            }
            let Some(ns) = objc2_app_kit::NSEvent::eventWithCGEvent(event_ref) else {
                return event.as_ptr();
            };
            if ns.subtype().0 != nx::SUBTYPE_AUX_CONTROL_BUTTONS {
                return event.as_ptr();
            }
            let Some((code, down)) = media(ns.data1()) else { return event.as_ptr() };
            let _sent = state.chords.send(Chord { code, down, mods: Mods::empty() });
            return std::ptr::null_mut();
        }
        let down = kind == CGEventType::KeyDown;
        if !down && kind != CGEventType::KeyUp {
            return event.as_ptr();
        }
        let keycode =
            CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode);
        let Ok(keycode) = u16::try_from(keycode) else { return event.as_ptr() };
        let mods = mods(CGEvent::flags(Some(event_ref)));
        match state.filter.borrow_mut().key(keycode, down, mods, armed()) {
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
    use std::cell::RefCell;
    use std::rc::Rc;

    use slopty_proto::input::{KeyCode, Mods};

    use super::{
        Chord, Filter, HotkeyLayer, Hotkeys, HotkeysOff, Taking, VK_3, VK_LEFT, VK_SPACE, VK_TAB,
        Verdict, chord, media,
    };

    /// A hotkey layer that records what it was set to, and may refuse.
    #[derive(Clone, Default)]
    struct Layer {
        set: Rc<RefCell<Vec<bool>>>,
        refuse: bool,
    }

    impl HotkeyLayer for Layer {
        type Token = ();

        fn off(&self) -> Option<()> {
            self.set.borrow_mut().push(false);
            (!self.refuse).then_some(())
        }

        fn on(&self, (): ()) {
            self.set.borrow_mut().push(true);
        }
    }

    /// The layer goes off with the guard and back on when it is dropped, and when a panic
    /// unwinds past it; a layer the WindowServer would not turn off is left alone.
    #[test]
    fn the_hotkey_mode_is_restored_on_disarm_and_drop() {
        let layer = Layer::default();
        let off = HotkeysOff::new(layer.clone());
        assert!(off.is_some());
        assert_eq!(*layer.set.borrow(), [false]);
        drop(off);
        assert_eq!(*layer.set.borrow(), [false, true], "on again once dropped");

        let unwound = layer.clone();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _off = HotkeysOff::new(unwound);
            panic!("the app fell over with a tile armed");
        }));
        assert!(panicked.is_err());
        assert_eq!(*layer.set.borrow(), [false, true, false, true], "on again after a panic");

        let refused = Layer { refuse: true, ..Layer::default() };
        assert!(HotkeysOff::new(refused.clone()).is_none());
        assert_eq!(*refused.set.borrow(), [false], "refused: nothing to turn back");
    }

    /// Another app coming to the front turns the layer back on at once, though the view still
    /// wants the shortcuts; coming back turns it off again; disarming while away sets nothing.
    #[test]
    fn leaving_the_app_turns_the_layer_back_on_at_once() {
        let layer = Layer::default();
        let mut hotkeys = Hotkeys::new(Some(layer.clone()), true);
        assert_eq!(hotkeys.arm(true), Taking::Every);
        hotkeys.app_active(false);
        assert_eq!(*layer.set.borrow(), [false, true], "on again the moment the app left");
        assert_eq!(hotkeys.taking(), Taking::Off);
        hotkeys.app_active(true);
        assert_eq!(*layer.set.borrow(), [false, true, false], "off again on the way back");
        hotkeys.app_active(false);
        assert_eq!(hotkeys.arm(false), Taking::Off);
        hotkeys.app_active(true);
        assert_eq!(*layer.set.borrow(), [false, true, false, true], "disarmed: stays on");
    }

    /// A WindowServer that keeps its layer, or a macOS without the calls, leaves the tap's own
    /// list to take the chords: the fallback follows the guard actually held, not the lookup.
    #[test]
    fn a_refused_layer_falls_back_to_the_chord_list() {
        let refused = Layer { refuse: true, ..Layer::default() };
        let mut hotkeys = Hotkeys::new(Some(refused), true);
        assert_eq!(hotkeys.arm(true), Taking::Chords);
        let mut missing = Hotkeys::<Layer>::new(None, true);
        assert_eq!(missing.arm(true), Taking::Chords);
        assert_eq!(missing.arm(false), Taking::Off);
    }

    /// Play/pause and the track keys, as an Apple keyboard sends them, go to the worker with
    /// their state; volume, mute and brightness stay here.
    #[test]
    fn media_keys_go_and_volume_stays() {
        let data1 = |key: isize, down: bool| (key << 16) | (if down { 0x0a } else { 0x0b } << 8);
        assert_eq!(media(data1(16, true)), Some((KeyCode::MediaPlayPause, true)));
        assert_eq!(media(data1(16, false)), Some((KeyCode::MediaPlayPause, false)));
        assert_eq!(media(data1(19, true)), Some((KeyCode::MediaTrackNext, true)), "FAST");
        assert_eq!(media(data1(17, true)), Some((KeyCode::MediaTrackNext, true)), "NEXT");
        assert_eq!(media(data1(20, true)), Some((KeyCode::MediaTrackPrevious, true)), "REWIND");
        for local in [0, 1, 2, 3, 7] {
            // NX_KEYTYPE_SOUND_UP, _DOWN, BRIGHTNESS_UP, _DOWN, MUTE.
            assert_eq!(media(data1(local, true)), None, "{local}");
        }
    }

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

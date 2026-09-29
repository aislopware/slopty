//! A chord that reaches the app from any other app: the quick terminal's.
//!
//! The chord is read from the settings in the palette's key syntax (`` ctrl-` ``) by
//! [`Chord::parse`], which is pure. On the Mac, [`Hotkey`] registers it with the Carbon Event
//! Manager's `RegisterEventHotKey`: the window server hands the chord to this app whichever app is
//! in front, and it never reaches that app. Unlike a session event tap (`system_keys`), a
//! registered hot key needs no Accessibility grant; it sees that one chord and nothing else
//! typed (`docs/decisions/ui.md`, "A quick terminal slides down from the top of the screen").

/// Why a chord from the settings was not taken.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ChordError {
    /// A word that is neither a modifier nor a key the chord can name.
    UnknownKey(String),
    /// No ⌘ or ⌃ with a key that types: the chord would take a letter or a symbol from every
    /// app, and macOS refuses a hot key with only ⌥ or ⌥⇧.
    NeedsCommandOrControl,
}

impl std::fmt::Display for ChordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKey(word) => write!(f, "no key is called \"{word}\""),
            Self::NeedsCommandOrControl => f.write_str("a chord needs cmd or ctrl, or an F key"),
        }
    }
}

impl std::error::Error for ChordError {}

/// A key and its modifiers, as the Carbon Event Manager names them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Chord {
    /// The key as the palette's syntax names it (`` ` ``, `space`, `f12`).
    pub key: &'static str,
    /// The key's virtual code (`kVK_*`): a place on the keyboard, the ANSI layout's.
    pub keycode: u16,
    /// ⌘.
    pub cmd: bool,
    /// ⌃.
    pub ctrl: bool,
    /// ⌥.
    pub alt: bool,
    /// ⇧.
    pub shift: bool,
}

impl Chord {
    /// Read a chord to register from any app: [`Chord::read`], with ⌘ or ⌃ unless the key is
    /// an F key.
    ///
    /// # Errors
    ///
    /// A word that names no key, or a chord without ⌘ or ⌃ on a key other than an F key.
    pub fn parse(text: &str) -> Result<Self, ChordError> {
        let chord = Self::read(text)?;
        if !(chord.cmd || chord.ctrl || is_function_key(chord.keycode)) {
            return Err(ChordError::NeedsCommandOrControl);
        }
        Ok(chord)
    }

    /// Read a chord in the palette's key syntax: modifiers (`cmd`, `ctrl`, `alt`, `shift`, and
    /// the Mac's names for them) and one key, joined by `-` (`` ctrl-` ``, `cmd-alt-space`,
    /// `f12`, `ctrl--`, `escape`). Case and surrounding space do not matter. Any modifiers go,
    /// none included: the app's own key bindings read their chords with it.
    ///
    /// # Errors
    ///
    /// A word that names no key.
    pub fn read(text: &str) -> Result<Self, ChordError> {
        let text = text.trim().to_ascii_lowercase();
        let (mods, key) = match text.strip_suffix("--") {
            Some(mods) => (mods, "-"),
            None => text.rsplit_once('-').unwrap_or(("", text.as_str())),
        };
        let key = match key {
            "return" => "enter",
            "esc" => "escape",
            other => other,
        };
        let (key, keycode) = KEYS
            .iter()
            .find(|(name, _)| *name == key)
            .copied()
            .ok_or_else(|| ChordError::UnknownKey(key.to_owned()))?;
        let mut chord = Self { key, keycode, cmd: false, ctrl: false, alt: false, shift: false };
        for word in mods.split('-').filter(|w| !w.is_empty()) {
            match word {
                "cmd" | "command" | "super" => chord.cmd = true,
                "ctrl" | "control" => chord.ctrl = true,
                "alt" | "option" | "opt" => chord.alt = true,
                "shift" => chord.shift = true,
                other => return Err(ChordError::UnknownKey(other.to_owned())),
            }
        }
        Ok(chord)
    }

    /// The chord in the palette's syntax as GPUI reads it: `ctrl-alt-shift-cmd-` and the key.
    #[must_use]
    pub fn keys(self) -> String {
        let mods =
            [(self.ctrl, "ctrl-"), (self.alt, "alt-"), (self.shift, "shift-"), (self.cmd, "cmd-")];
        let mut out: String = mods.iter().filter(|(on, _)| *on).map(|(_, word)| *word).collect();
        out.push_str(self.key);
        out
    }

    /// The Carbon modifier mask (`cmdKey`, `shiftKey`, `optionKey`, `controlKey` from
    /// `<HIToolbox/Events.h>`, which objc2 does not bind).
    #[must_use]
    pub const fn carbon_modifiers(self) -> u32 {
        let mut mask = 0;
        if self.cmd {
            mask |= 1 << 8;
        }
        if self.shift {
            mask |= 1 << 9;
        }
        if self.alt {
            mask |= 1 << 11;
        }
        if self.ctrl {
            mask |= 1 << 12;
        }
        mask
    }
}

fn is_function_key(keycode: u16) -> bool {
    KEYS.iter().any(|(name, code)| *code == keycode && name.len() > 1 && name.starts_with('f'))
}

/// The keys a chord can name, in the palette's syntax, with their virtual codes (`kVK_*`,
/// `<HIToolbox/Events.h>`, which objc2 does not bind).
const KEYS: [(&str, u16); 81] = [
    ("a", 0x00),
    ("s", 0x01),
    ("d", 0x02),
    ("f", 0x03),
    ("h", 0x04),
    ("g", 0x05),
    ("z", 0x06),
    ("x", 0x07),
    ("c", 0x08),
    ("v", 0x09),
    ("b", 0x0b),
    ("q", 0x0c),
    ("w", 0x0d),
    ("e", 0x0e),
    ("r", 0x0f),
    ("y", 0x10),
    ("t", 0x11),
    ("1", 0x12),
    ("2", 0x13),
    ("3", 0x14),
    ("4", 0x15),
    ("6", 0x16),
    ("5", 0x17),
    ("=", 0x18),
    ("9", 0x19),
    ("7", 0x1a),
    ("-", 0x1b),
    ("8", 0x1c),
    ("0", 0x1d),
    ("]", 0x1e),
    ("o", 0x1f),
    ("u", 0x20),
    ("[", 0x21),
    ("i", 0x22),
    ("p", 0x23),
    ("enter", 0x24),
    ("l", 0x25),
    ("j", 0x26),
    ("'", 0x27),
    ("k", 0x28),
    (";", 0x29),
    ("\\", 0x2a),
    (",", 0x2b),
    ("/", 0x2c),
    ("n", 0x2d),
    ("m", 0x2e),
    (".", 0x2f),
    ("tab", 0x30),
    ("space", 0x31),
    ("`", 0x32),
    ("backspace", 0x33),
    ("escape", 0x35),
    ("home", 0x73),
    ("pageup", 0x74),
    ("delete", 0x75),
    ("end", 0x77),
    ("pagedown", 0x79),
    ("left", 0x7b),
    ("right", 0x7c),
    ("down", 0x7d),
    ("up", 0x7e),
    ("f1", 0x7a),
    ("f2", 0x78),
    ("f3", 0x63),
    ("f4", 0x76),
    ("f5", 0x60),
    ("f6", 0x61),
    ("f7", 0x62),
    ("f8", 0x64),
    ("f9", 0x65),
    ("f10", 0x6d),
    ("f11", 0x67),
    ("f12", 0x6f),
    ("f13", 0x69),
    ("f14", 0x6b),
    ("f15", 0x71),
    ("f16", 0x6a),
    ("f17", 0x40),
    ("f18", 0x4f),
    ("f19", 0x50),
    ("f20", 0x5a),
];

#[cfg(target_os = "macos")]
pub use carbon::{Hotkey, HotkeyError};

#[cfg(target_os = "macos")]
mod carbon {
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    use tokio::sync::mpsc::UnboundedSender;

    use super::Chord;

    // The Carbon Event Manager (`<HIToolbox/CarbonEvents.h>`, `<HIToolbox/Events.h>`), which
    // objc2 does not bind. Every `*Ref` is an opaque pointer; `ItemCount` and `ByteCount` are
    // `unsigned long`, a `usize` on Apple silicon; `OSType` and `OptionBits` are `UInt32`.
    type OsStatus = i32;
    type EventRef = *mut c_void;
    type EventTargetRef = *mut c_void;
    type EventHandlerRef = *mut c_void;
    type EventHandlerCallRef = *mut c_void;
    type EventHotKeyRef = *mut c_void;
    type EventHandlerProc =
        unsafe extern "C-unwind" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OsStatus;

    /// `EventHotKeyID`.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct EventHotKeyId {
        signature: u32,
        id: u32,
    }

    /// `EventTypeSpec`.
    #[repr(C)]
    struct EventTypeSpec {
        event_class: u32,
        event_kind: u32,
    }

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C-unwind" {
        fn GetApplicationEventTarget() -> EventTargetRef;
        fn InstallEventHandler(
            target: EventTargetRef,
            handler: EventHandlerProc,
            num_types: usize,
            list: *const EventTypeSpec,
            user_data: *mut c_void,
            out_ref: *mut EventHandlerRef,
        ) -> OsStatus;
        fn RemoveEventHandler(handler: EventHandlerRef) -> OsStatus;
        fn RegisterEventHotKey(
            key_code: u32,
            modifiers: u32,
            id: EventHotKeyId,
            target: EventTargetRef,
            options: u32,
            out_ref: *mut EventHotKeyRef,
        ) -> OsStatus;
        fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OsStatus;
        fn GetEventParameter(
            event: EventRef,
            name: u32,
            desired_type: u32,
            actual_type: *mut u32,
            buffer_size: usize,
            actual_size: *mut usize,
            data: *mut c_void,
        ) -> OsStatus;
    }

    /// `noErr`.
    const NO_ERR: OsStatus = 0;
    /// `eventNotHandledErr`: the event goes on to the next handler.
    const EVENT_NOT_HANDLED: OsStatus = -9874;
    /// `eventHotKeyExistsErr`: another app registered the chord first.
    const HOT_KEY_EXISTS: OsStatus = -9878;
    /// `kEventClassKeyboard`, `'keyb'`.
    const CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
    /// `kEventHotKeyPressed`.
    const HOT_KEY_PRESSED: u32 = 5;
    /// `kEventParamDirectObject`, `'----'`.
    const PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
    /// `typeEventHotKeyID`, `'hkid'`.
    const TYPE_HOT_KEY_ID: u32 = u32::from_be_bytes(*b"hkid");
    /// The signature this app's hot keys carry.
    const SIGNATURE: u32 = u32::from_be_bytes(*b"SLPT");

    /// Why a chord could not be registered.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum HotkeyError {
        /// Not on the main thread, whose run loop delivers hot keys.
        NotMain,
        /// Another app holds the chord.
        Taken,
        /// The Event Manager said no, with this `OSStatus`.
        Refused(i32),
    }

    impl std::fmt::Display for HotkeyError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::NotMain => f.write_str("a hot key is registered on the main thread"),
                Self::Taken => f.write_str("another app already uses it"),
                Self::Refused(status) => write!(f, "macOS refused it ({status})"),
            }
        }
    }

    impl std::error::Error for HotkeyError {}

    /// What the handler reads: which hot key is ours, and where a press goes.
    struct State {
        id: u32,
        pressed: UnboundedSender<Instant>,
    }

    /// A registered chord: each press sends the moment it arrived to the channel. It goes
    /// when dropped. Made, used and dropped on the main thread.
    #[derive(Debug)]
    pub struct Hotkey {
        hot_key: NonNull<c_void>,
        handler: NonNull<c_void>,
        state: NonNull<c_void>,
    }

    impl Hotkey {
        /// Register `chord` for this app; every press sends its arrival to `pressed`.
        ///
        /// # Errors
        ///
        /// Off the main thread, when another app holds the chord, or when macOS refuses it.
        pub fn register(
            chord: Chord,
            pressed: UnboundedSender<Instant>,
        ) -> Result<Self, HotkeyError> {
            static NEXT: AtomicU32 = AtomicU32::new(1);
            if objc2::MainThreadMarker::new().is_none() {
                return Err(HotkeyError::NotMain);
            }
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let state = NonNull::from(Box::leak(Box::new(State { id, pressed }))).cast::<c_void>();
            let free_state = || {
                // SAFETY: `state` came from `Box::leak` above, and no handler holds it.
                drop(unsafe { Box::from_raw(state.cast::<State>().as_ptr()) });
            };
            // SAFETY: the Event Manager's own target for this app, fetched on the main thread.
            let target = unsafe { GetApplicationEventTarget() };
            let spec = EventTypeSpec { event_class: CLASS_KEYBOARD, event_kind: HOT_KEY_PRESSED };
            let mut handler: EventHandlerRef = std::ptr::null_mut();
            // SAFETY: `on_hot_key` matches `EventHandlerProcPtr`, `spec` outlives the call (the
            // Event Manager copies the list), and `state` outlives the handler: `Drop` removes
            // the handler before it frees `state`.
            let status = unsafe {
                InstallEventHandler(
                    target,
                    on_hot_key,
                    1,
                    &raw const spec,
                    state.as_ptr(),
                    &raw mut handler,
                )
            };
            let Some(handler) = NonNull::new(handler).filter(|_| status == NO_ERR) else {
                free_state();
                return Err(HotkeyError::Refused(status));
            };
            let hot_key_id = EventHotKeyId { signature: SIGNATURE, id };
            let mut hot_key: EventHotKeyRef = std::ptr::null_mut();
            // SAFETY: plain values and an out pointer to a local, on the main thread.
            let status = unsafe {
                RegisterEventHotKey(
                    u32::from(chord.keycode),
                    chord.carbon_modifiers(),
                    hot_key_id,
                    target,
                    0,
                    &raw mut hot_key,
                )
            };
            let Some(hot_key) = NonNull::new(hot_key).filter(|_| status == NO_ERR) else {
                // SAFETY: the handler installed above, removed once; nothing calls it after.
                let _removed = unsafe { RemoveEventHandler(handler.as_ptr()) };
                free_state();
                return Err(if status == HOT_KEY_EXISTS {
                    HotkeyError::Taken
                } else {
                    HotkeyError::Refused(status)
                });
            };
            Ok(Self { hot_key, handler, state })
        }
    }

    impl Drop for Hotkey {
        fn drop(&mut self) {
            // SAFETY: the hot key `register` made, let go once, on the main thread `Hotkey`
            // lives on.
            let _gone = unsafe { UnregisterEventHotKey(self.hot_key.as_ptr()) };
            // SAFETY: the handler `register` installed, removed once; nothing calls it after.
            let _removed = unsafe { RemoveEventHandler(self.handler.as_ptr()) };
            // SAFETY: with the handler removed nothing reads `state`, which came from
            // `Box::leak` in `register`.
            drop(unsafe { Box::from_raw(self.state.cast::<State>().as_ptr()) });
        }
    }

    /// A hot key was pressed: one of ours sends the moment on; any other goes on to the next
    /// handler.
    unsafe extern "C-unwind" fn on_hot_key(
        _call: EventHandlerCallRef,
        event: EventRef,
        user_data: *mut c_void,
    ) -> OsStatus {
        let arrived = Instant::now();
        // SAFETY: `user_data` is the `State` `Hotkey::register` leaked, alive until the handler
        // is removed; the Event Manager calls handlers on the main thread only.
        let Some(state) = (unsafe { user_data.cast::<State>().as_ref() }) else {
            return EVENT_NOT_HANDLED;
        };
        let mut id = EventHotKeyId::default();
        // SAFETY: a hot key event carries its `EventHotKeyID` as the direct object; `id` is a
        // buffer of exactly its size.
        let status = unsafe {
            GetEventParameter(
                event,
                PARAM_DIRECT_OBJECT,
                TYPE_HOT_KEY_ID,
                std::ptr::null_mut(),
                size_of::<EventHotKeyId>(),
                std::ptr::null_mut(),
                (&raw mut id).cast::<c_void>(),
            )
        };
        if status != NO_ERR || id.signature != SIGNATURE || id.id != state.id {
            return EVENT_NOT_HANDLED;
        }
        let _sent = state.pressed.send(arrived);
        NO_ERR
    }
}

#[cfg(test)]
mod tests {
    use super::{Chord, ChordError};

    /// The palette's syntax reads as the keyboard's places: ⌃\` is `kVK_ANSI_Grave` with
    /// `controlKey`, a trailing `--` is the minus key, F keys stand alone, and a chord that
    /// would take typing from every app is refused.
    #[test]
    fn a_chord_reads_in_the_palettes_syntax() {
        let grave = Chord::parse("ctrl-`").expect("the default");
        assert_eq!((grave.keycode, grave.carbon_modifiers()), (0x32, 1 << 12));
        let space = Chord::parse(" Cmd-Alt-Space ").expect("case and space");
        assert_eq!((space.keycode, space.carbon_modifiers()), (0x31, (1 << 8) | (1 << 11)));
        assert_eq!(Chord::parse("ctrl--").map(|c| c.keycode), Ok(0x1b), "the minus key");
        assert_eq!(Chord::parse("f12").map(|c| c.keycode), Ok(0x6f), "an F key alone");
        assert_eq!(Chord::parse("shift-f1").map(|c| c.keycode), Ok(0x7a));
        assert_eq!(Chord::parse("alt-a"), Err(ChordError::NeedsCommandOrControl));
        assert_eq!(Chord::parse("`"), Err(ChordError::NeedsCommandOrControl));
        assert_eq!(Chord::parse("hyper-a"), Err(ChordError::UnknownKey("hyper".into())));
        assert_eq!(Chord::parse("ctrl-f21"), Err(ChordError::UnknownKey("f21".into())));
        assert_eq!(Chord::parse("ctrl-"), Err(ChordError::UnknownKey(String::new())));
        let spelled = Chord::parse("Option-Command-Return").expect("the Mac's names");
        assert_eq!(spelled.keys(), "alt-cmd-enter", "written back as GPUI reads it");
        assert_eq!(grave.keys(), "ctrl-`");
        // The app's own bindings take any chord: a bare key, ⌥ alone, ⇧ on a page key.
        assert_eq!(Chord::read("escape").map(Chord::keys), Ok("escape".to_owned()));
        assert_eq!(Chord::read("Shift-PageUp").map(Chord::keys), Ok("shift-pageup".to_owned()));
        assert_eq!(Chord::read("alt-a").map(Chord::keys), Ok("alt-a".to_owned()));
        assert_eq!(Chord::read("cmd-wat"), Err(ChordError::UnknownKey("wat".into())));
    }

    /// The Event Manager delivers hot keys on the main thread's run loop, so a chord is
    /// registered there or not at all; a test thread is refused before Carbon is touched.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_hot_key_is_registered_on_the_main_thread_only() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let chord = Chord::parse("ctrl-`").expect("the default");
        assert_eq!(super::Hotkey::register(chord, tx).map(drop), Err(super::HotkeyError::NotMain));
    }
}

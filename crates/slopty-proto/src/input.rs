//! Keyboard and pointer events as the client observed them.
//!
//! The client sends *what happened* (physical key, modifiers, the text the OS produced); the host
//! turns it into bytes with the engine's key encoder, which knows every terminal mode. Names follow
//! the W3C UI Events `code` values so the mapping to the engine is one-to-one.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

/// Every physical key the engine knows.
///
/// One name per W3C `KeyboardEvent.code` value plus the numpad/browser extras ghostty recognises.
/// Exported so the engine's mapping is generated from the same list and checked exhaustively.
#[macro_export]
macro_rules! for_each_key_code {
    ($callback:ident) => {
        $callback! {
            Unidentified, Backquote, Backslash, BracketLeft, BracketRight, Comma, Digit0, Digit1,
            Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
            Equal, IntlBackslash, IntlRo, IntlYen, A, B, C, D,
            E, F, G, H, I, J, K, L,
            M, N, O, P, Q, R, S, T,
            U, V, W, X, Y, Z, Minus, Period,
            Quote, Semicolon, Slash, AltLeft, AltRight, Backspace, CapsLock, ContextMenu,
            ControlLeft, ControlRight, Enter, MetaLeft, MetaRight, ShiftLeft, ShiftRight, Space,
            Tab, Convert, KanaMode, NonConvert, Delete, End, Help, Home,
            Insert, PageDown, PageUp, ArrowDown, ArrowLeft, ArrowRight, ArrowUp, NumLock,
            Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7,
            Numpad8, Numpad9, NumpadAdd, NumpadBackspace, NumpadClear, NumpadClearEntry, NumpadComma, NumpadDecimal,
            NumpadDivide, NumpadEnter, NumpadEqual, NumpadMemoryAdd, NumpadMemoryClear, NumpadMemoryRecall, NumpadMemoryStore, NumpadMemorySubtract,
            NumpadMultiply, NumpadParenLeft, NumpadParenRight, NumpadSubtract, NumpadSeparator, NumpadUp, NumpadDown, NumpadRight,
            NumpadLeft, NumpadBegin, NumpadHome, NumpadEnd, NumpadInsert, NumpadDelete, NumpadPageUp, NumpadPageDown,
            Escape, F1, F2, F3, F4, F5, F6, F7,
            F8, F9, F10, F11, F12, F13, F14, F15,
            F16, F17, F18, F19, F20, F21, F22, F23,
            F24, F25, Fn, FnLock, PrintScreen, ScrollLock, Pause, BrowserBack,
            BrowserFavorites, BrowserForward, BrowserHome, BrowserRefresh, BrowserSearch, BrowserStop, Eject, LaunchApp1,
            LaunchApp2, LaunchMail, MediaPlayPause, MediaSelect, MediaStop, MediaTrackNext, MediaTrackPrevious, Power,
            Sleep, AudioVolumeDown, AudioVolumeMute, AudioVolumeUp, WakeUp, Copy, Cut, Paste,
        }
    };
}

macro_rules! define_key_code {
    ($($name:ident),* $(,)?) => {
        /// Physical key (W3C `KeyboardEvent.code`), same set and order as the engine's.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
        #[expect(missing_docs, reason = "W3C key names are self-describing")]
        pub enum KeyCode {
            #[default]
            $($name,)*
        }

        impl KeyCode {
            /// Every key, in wire order.
            pub const ALL: &[Self] = &[$(Self::$name),*];
        }
    };
}
for_each_key_code!(define_key_code);

bitflags! {
    /// Modifier state. The `*_RIGHT` bits say which side, when the OS reports it.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct Mods: u16 {
        /// Shift.
        const SHIFT = 1 << 0;
        /// Option / Alt.
        const ALT = 1 << 1;
        /// Control.
        const CTRL = 1 << 2;
        /// Command / Super.
        const SUPER = 1 << 3;
        /// Caps Lock is on.
        const CAPS_LOCK = 1 << 4;
        /// Num Lock is on.
        const NUM_LOCK = 1 << 5;
        /// Right shift (only meaningful with `SHIFT`).
        const SHIFT_RIGHT = 1 << 6;
        /// Right alt.
        const ALT_RIGHT = 1 << 7;
        /// Right control.
        const CTRL_RIGHT = 1 << 8;
        /// Right super.
        const SUPER_RIGHT = 1 << 9;
    }
}

/// Press, repeat, or release.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum KeyAction {
    /// Key went down.
    Press,
    /// OS auto-repeat.
    Repeat,
    /// Key went up (only forwarded when the kitty protocol asks for it).
    Release,
}

/// One key event, shaped exactly like ghostty's own view constructs it.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct KeyEvent {
    /// Client-assigned sequence number; the host echoes the highest applied one in each frame so
    /// the prediction engine can retire its speculation.
    pub seq: u64,
    /// Press / repeat / release.
    pub action: KeyAction,
    /// Physical key.
    pub code: KeyCode,
    /// Modifiers held.
    pub mods: Mods,
    /// Modifiers the OS already consumed to produce `text` (e.g. Shift for `A`). Excludes Ctrl
    /// and Cmd, which the encoder handles itself.
    pub consumed_mods: Mods,
    /// Text produced by the keyboard layout, without control folding. `None` for non-text keys.
    pub text: Option<String>,
    /// The key's codepoint with no modifiers applied (`a` for Shift+A), for kitty alternates.
    pub unshifted: Option<char>,
    /// Inside an IME composition; the engine must not encode it.
    pub composing: bool,
}

/// Pointer button.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum MouseButton {
    /// Primary.
    Left,
    /// Secondary.
    Right,
    /// Wheel click.
    Middle,
    /// Back (button 8).
    Back,
    /// Forward (button 9).
    Forward,
}

/// Pointer action.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum MouseAction {
    /// Button down.
    Press,
    /// Button up.
    Release,
    /// Moved (with or without a button held).
    Motion,
    /// Wheel step: positive is up/left, one unit per notch or per row of precise scroll.
    Wheel {
        /// Vertical rows (positive = up).
        rows: i16,
        /// Horizontal columns (positive = left).
        cols: i16,
    },
}

/// One pointer event in terminal cell space, plus the pixel offset the SGR-pixel mode needs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct MouseEvent {
    /// Action.
    pub action: MouseAction,
    /// Button involved, if any (`None` for motion without a button).
    pub button: Option<MouseButton>,
    /// Modifiers.
    pub mods: Mods,
    /// Column.
    pub col: u16,
    /// Row.
    pub row: u16,
    /// Pixel x within the terminal content area, in the client's cell metrics.
    pub px: u32,
    /// Pixel y.
    pub py: u32,
}

/// Client cell geometry, so the host can encode pixel-mode mouse reports and `TIOCSWINSZ` pixel
/// fields identically to a local terminal.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub struct CellMetrics {
    /// Cell width in device pixels.
    pub cell_width: u16,
    /// Cell height in device pixels.
    pub cell_height: u16,
}

//! The keyboard of a remote window or display: keys by their place on the keyboard, text this
//! device composed, Caps Lock's state, and this device's input source
//! (`docs/decisions/input.md`, "Keys go by position").
//!
//! A key goes to the worker as its position, never as the character this device's layout puts
//! there: the worker's layout makes the character, so ⌘Q is the Q key on both Macs whatever the
//! layouts, and a dead key or an input method composes in the remote app. For that the worker
//! types under this device's input source: the view tells it the source when it takes the
//! keyboard and whenever the person switches, and the worker answers whether it took it.
//!
//! - *Taken* (the worker took the source): the keys without ⌘ or ⌃ are taken off the app before the
//!   text system can compose them (`slopty_platform::keyboard`) and sent as they are; a chord
//!   reaches the view through GPUI, whose bindings come first, and goes by the position read off
//!   the event.
//! - *Composed* (not yet, or never: an input method the worker lacks, an iPad): the text system
//!   here composes, dead keys and input methods included, and what it commits goes as
//!   [`ScreenInput::Text`]; named keys still go by position, and a ⌘ or ⌃ chord names its
//!   character, which the worker presses where its own layout types it, else at the character's
//!   place on a US keyboard (`docs/decisions/input.md`, "A shortcut goes by its character").

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Context, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ModifiersChangedEvent, Task, Window,
};
use slopty_platform::keyboard::{KeyKind, NativeKey, Taken};
use slopty_proto::input::{KeyAction, KeyCode, Mods};
use slopty_proto::screen::{MediaKey, ScreenInput};
use tokio::sync::Notify;

use super::{ScreenView, is_paste_chord};
use crate::keys;

/// What the view asks the platform about the keyboard. A seam: tests drive a stand-in.
pub trait KeyPlatform {
    /// The key event being dispatched now, with its position and its modifiers' sides.
    fn native(&self) -> Option<NativeKey>;
    /// This device's keyboard input source, when it has one a Mac can select.
    fn source(&self) -> Option<String>;
    /// Whether Caps Lock is on.
    fn caps_lock(&self) -> Option<bool>;
    /// Take the keys off the app before its text system composes them; `wake` is called as
    /// each comes.
    fn take(&self, wake: Box<dyn Fn()>) -> Option<Taken>;
    /// Call `changed` when the person switches input source, while the answer lives.
    fn watch(&self, changed: Box<dyn Fn()>) -> Option<Box<dyn std::any::Any>>;
}

/// The device's own keyboard.
struct System;

impl KeyPlatform for System {
    #[cfg(target_os = "macos")]
    fn native(&self) -> Option<NativeKey> {
        slopty_platform::keyboard::current()
    }

    #[cfg(target_os = "macos")]
    fn source(&self) -> Option<String> {
        slopty_platform::input_source::current()
    }

    #[cfg(target_os = "macos")]
    fn caps_lock(&self) -> Option<bool> {
        slopty_platform::keyboard::caps_lock()
    }

    #[cfg(target_os = "macos")]
    fn take(&self, wake: Box<dyn Fn()>) -> Option<Taken> {
        slopty_platform::keyboard::take_keys(wake)
    }

    #[cfg(target_os = "macos")]
    fn watch(&self, changed: Box<dyn Fn()>) -> Option<Box<dyn std::any::Any>> {
        let watch = slopty_platform::input_source::Watch::new(changed)?;
        Some(Box::new(watch))
    }

    // An iPad names neither positions nor a source a Mac can select: its text system composes
    // and chords go by their character's place on a US keyboard.
    #[cfg(not(target_os = "macos"))]
    fn native(&self) -> Option<NativeKey> {
        None
    }

    #[cfg(not(target_os = "macos"))]
    fn source(&self) -> Option<String> {
        None
    }

    #[cfg(not(target_os = "macos"))]
    fn caps_lock(&self) -> Option<bool> {
        None
    }

    #[cfg(not(target_os = "macos"))]
    fn take(&self, _wake: Box<dyn Fn()>) -> Option<Taken> {
        None
    }

    #[cfg(not(target_os = "macos"))]
    fn watch(&self, _changed: Box<dyn Fn()>) -> Option<Box<dyn std::any::Any>> {
        None
    }
}

/// A keyboard that answers nothing: under test, where no real key is ever read or taken.
struct Inert;

impl KeyPlatform for Inert {
    fn native(&self) -> Option<NativeKey> {
        None
    }

    fn source(&self) -> Option<String> {
        None
    }

    fn caps_lock(&self) -> Option<bool> {
        None
    }

    fn take(&self, _wake: Box<dyn Fn()>) -> Option<Taken> {
        None
    }

    fn watch(&self, _changed: Box<dyn Fn()>) -> Option<Box<dyn std::any::Any>> {
        None
    }
}

/// The view's keyboard state.
pub(super) struct Keyboard {
    platform: Rc<dyn KeyPlatform>,
    /// The view has the keyboard.
    focused: bool,
    /// The input source last told to the worker.
    told: Option<String>,
    /// The worker answered that it types under `told`.
    applied: bool,
    /// Caps Lock's state as last sent.
    caps: Option<bool>,
    /// The keys taken off the app while the worker types under this device's source.
    taken: Option<Taken>,
    /// Wakes the view for taken keys and source switches.
    wake: Rc<Notify>,
    /// The person switched input source since the view last looked.
    switched: Rc<Cell<bool>>,
    /// Holds the input-source watch.
    watch: Option<Box<dyn std::any::Any>>,
    /// Drains taken keys and hears switches while the view lives.
    woken: Option<Task<()>>,
    /// Gives the worker's source back once the view has been without the keyboard for
    /// [`RELEASE_AFTER`]; taking the keyboard again drops it.
    releasing: Option<Task<()>>,
    /// Chords sent by the character's place rather than their own: the key's own place, and
    /// the key sent, so the release goes to the same key.
    chorded: Vec<(KeyCode, KeyCode)>,
}

/// How long a view goes without the keyboard before its claim on the worker's input source
/// goes. Hopping between tiles or opening the palette leaves the claim, so coming back takes
/// no switch; a tile left this long gives the person at the worker their own source back.
pub(super) const RELEASE_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

impl Keyboard {
    /// The keyboard of a new view: the device's own, or none under test.
    pub(super) fn new(cx: &Context<ScreenView>) -> Self {
        let platform: Rc<dyn KeyPlatform> =
            if cfg!(test) { Rc::new(Inert) } else { Rc::new(System) };
        let mut keyboard = Self {
            platform,
            focused: false,
            told: None,
            applied: false,
            caps: None,
            taken: None,
            wake: Rc::new(Notify::new()),
            switched: Rc::new(Cell::new(false)),
            watch: None,
            woken: None,
            releasing: None,
            chorded: Vec::new(),
        };
        keyboard.listen(cx);
        keyboard
    }

    /// Watch the platform's input source and wake the view for what it hears.
    fn listen(&mut self, cx: &Context<ScreenView>) {
        let (wake, switched) = (Rc::clone(&self.wake), Rc::clone(&self.switched));
        self.watch = self.platform.watch(Box::new(move || {
            switched.set(true);
            wake.notify_one();
        }));
        let wake = Rc::clone(&self.wake);
        self.woken = Some(cx.spawn(async move |this, cx| {
            loop {
                wake.notified().await;
                if this.update(cx, ScreenView::keyboard_woken).is_err() {
                    break;
                }
            }
        }));
    }

    /// Whether the worker composes: it took this device's source, and the keys are taken.
    const fn taking(&self) -> bool {
        self.applied && self.taken.is_some()
    }
}

/// The key a native event is, by its position; a position no key has falls back to the
/// character's place on a US keyboard.
fn code_of(native: Option<NativeKey>, keystroke: &Keystroke) -> (KeyCode, Mods) {
    native
        .and_then(|n| KeyCode::from_mac_vk(n.vk).map(|code| (code, n.mods)))
        .unwrap_or_else(|| (keys::key_code(&keystroke.key), keys::screen_mods(keystroke.modifiers)))
}

/// The character a chord's key names, as the wire carries it (`ScreenInput::Key::chord`): the
/// key's own character, lowercased, when it is one character; a named key (`left`, `f1`)
/// names none.
fn chord_char(key: &str) -> Option<String> {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c.to_lowercase().collect()),
        _ => None,
    }
}

/// A key of the numeric keypad.
const fn is_keypad(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Numpad0
            | KeyCode::Numpad1
            | KeyCode::Numpad2
            | KeyCode::Numpad3
            | KeyCode::Numpad4
            | KeyCode::Numpad5
            | KeyCode::Numpad6
            | KeyCode::Numpad7
            | KeyCode::Numpad8
            | KeyCode::Numpad9
            | KeyCode::NumpadAdd
            | KeyCode::NumpadSubtract
            | KeyCode::NumpadMultiply
            | KeyCode::NumpadDivide
            | KeyCode::NumpadDecimal
            | KeyCode::NumpadEqual
            | KeyCode::NumpadEnter
            | KeyCode::NumpadClear
    )
}

/// ⌘⌥⎋ (force quit) and ⌃⌘Q (lock the screen): the ways out that stay on this Mac.
fn stays_here(keystroke: &Keystroke) -> bool {
    let m = keystroke.modifiers;
    (m.platform && m.alt && !m.control && keystroke.key == "escape")
        || (m.platform && m.control && !m.alt && keystroke.key == "q")
}

/// The key of a media key's code.
const fn media_key(code: KeyCode) -> Option<MediaKey> {
    match code {
        KeyCode::MediaPlayPause => Some(MediaKey::PlayPause),
        KeyCode::MediaTrackNext => Some(MediaKey::Next),
        KeyCode::MediaTrackPrevious => Some(MediaKey::Previous),
        _ => None,
    }
}

impl ScreenView {
    /// Use `platform` for the keyboard from now on (tests' stand-ins).
    #[cfg(test)]
    pub(super) fn set_key_platform(&mut self, platform: Rc<dyn KeyPlatform>, cx: &Context<Self>) {
        self.keyboard.platform = platform;
        self.keyboard.listen(cx);
    }

    /// Taken keys waited, or the input source changed.
    fn keyboard_woken(&mut self, cx: &mut Context<Self>) {
        self.drain_taken();
        if self.keyboard.switched.take() && self.keyboard.focused {
            self.tell_source();
        }
        cx.notify();
    }

    /// The view took the keyboard: the worker hears this device's input source and Caps
    /// Lock's state.
    pub(super) fn keyboard_focused(&mut self) {
        self.keyboard.focused = true;
        self.keyboard.releasing = None;
        self.tell_source();
        self.keyboard.caps = None;
        if let Some(on) = self.keyboard.platform.caps_lock() {
            self.set_caps(on);
        }
    }

    /// The view let the keyboard go: nothing more is taken for it, and after [`RELEASE_AFTER`]
    /// without it the worker's source goes back.
    pub(super) fn keyboard_blurred(&mut self, cx: &Context<Self>) {
        self.drain_taken();
        self.keyboard.focused = false;
        self.keyboard.taken = None;
        if self.keyboard.told.is_some() && self.keyboard.releasing.is_none() {
            self.keyboard.releasing = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RELEASE_AFTER).await;
                let _gone = this.update(cx, |view, _cx| view.release_keyboard());
            }));
        }
    }

    /// The view has been without the keyboard for [`RELEASE_AFTER`]: its claim on the worker's
    /// source goes. The source stays told, so taking the keyboard again under it takes the keys
    /// at once; the worker holds them until its switch back is heard.
    fn release_keyboard(&mut self) {
        self.keyboard.releasing = None;
        if !self.keyboard.focused && self.keyboard.told.is_some() {
            self.send_input(ScreenInput::KeyboardReleased);
        }
    }

    /// Tell the worker this device's input source. A new one is composed here until the worker
    /// answers; the one it already took keeps being taken (the worker answers it at once, and
    /// says so should another client take the worker's source meanwhile), so the first keys
    /// after the tile takes the keyboard again go raw like the rest.
    fn tell_source(&mut self) {
        let Some(source) = self.keyboard.platform.source() else { return };
        if self.keyboard.told.as_deref() != Some(source.as_str()) {
            self.keyboard.applied = false;
        }
        self.keyboard.told = Some(source.clone());
        self.follow_taking();
        self.send_input(ScreenInput::KeyboardSource { source });
    }

    /// The worker's answer (`ScreenEvent::KeyboardSource`): whether it now types under
    /// `source`. An answer to a source since replaced is ignored.
    pub fn set_keyboard_source(&mut self, source: &str, applied: bool) {
        if self.keyboard.told.as_deref() == Some(source) {
            self.keyboard.applied = applied;
            self.follow_taking();
        }
    }

    /// Whether the worker composes this view's text: it took this device's input source.
    #[must_use]
    pub const fn worker_composes(&self) -> bool {
        self.keyboard.taking()
    }

    /// Take the keys while the view has the keyboard and the worker types under this device's
    /// source, and nothing is being composed here; let them be otherwise, sending what was taken
    /// first. A composition under way when the worker answers finishes here, and its text goes
    /// before the first key taken: taking its keys mid-word would strand the marked text.
    pub(super) fn follow_taking(&mut self) {
        let want = self.keyboard.focused && self.keyboard.applied && self.marked.is_none();
        if want && self.keyboard.taken.is_none() {
            let wake = Rc::clone(&self.keyboard.wake);
            self.keyboard.taken = self.keyboard.platform.take(Box::new(move || wake.notify_one()));
        } else if !want && self.keyboard.taken.is_some() {
            self.drain_taken();
            self.keyboard.taken = None;
        }
    }

    /// Send the keys taken since the last drain, in order: before anything else the view
    /// sends, so a click or a chord after a key never overtakes it.
    pub(super) fn drain_taken(&mut self) {
        let Some(taken) = self.keyboard.taken.as_ref() else { return };
        for key in taken.drain() {
            let Some(code) = KeyCode::from_mac_vk(key.vk) else { continue };
            match key.kind {
                KeyKind::Down => self.press_key(code, false, key.mods),
                KeyKind::Repeat => self.press_key(code, true, key.mods),
                KeyKind::Up => {
                    self.release_key(code, key.mods);
                }
                KeyKind::Flags => {}
            }
        }
    }

    /// Caps Lock is `on` here: the worker's follows, when it differs from what was sent.
    fn set_caps(&mut self, on: bool) {
        if self.keyboard.caps != Some(on) {
            self.keyboard.caps = Some(on);
            self.send_input(ScreenInput::Lock { caps: on });
        }
    }

    pub(super) fn key_down(&mut self, ev: &KeyDownEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if self.key_pressed(&ev.keystroke, ev.is_held, cx) {
            cx.stop_propagation();
        }
    }

    /// A key went down or repeats; whether the view took it. One left to the text system (a
    /// character while this device composes) reaches `replace_text_in_range` as text.
    pub(super) fn key_pressed(
        &mut self,
        keystroke: &Keystroke,
        repeat: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        self.drain_taken();
        if stays_here(keystroke) {
            return false;
        }
        let native = self.keyboard.platform.native().filter(|n| {
            matches!((n.kind, repeat), (KeyKind::Down, false) | (KeyKind::Repeat, true))
        });
        let (code, mods) = code_of(native, keystroke);
        // A keypad key is its place whatever it types: GPUI before the gpui-fast fork names
        // keypad 5 as `5`, which would otherwise go to the text system as the main row's.
        if !self.keyboard.taking() && keys::composes(keystroke) && !is_keypad(code) {
            // UIKit gives a character it did not type itself (⌥ with a letter) only as a key.
            if cfg!(target_os = "ios") {
                let text = keystroke.key_char.clone().filter(|t| !t.is_empty());
                return text.is_some_and(|text| {
                    self.send_input(ScreenInput::Text { text });
                    true
                });
            }
            return false;
        }
        if code == KeyCode::Unidentified {
            return false;
        }
        let (code, chord) = self.chord_code(code, keystroke);
        // Paste is the character this device's layout typed, wherever its key sits.
        if !repeat && is_paste_chord(keystroke) {
            self.push_clipboard(cx);
            self.hold_key(code);
            self.send_input(ScreenInput::PasteChord { code, mods });
            return true;
        }
        self.hold_key(code);
        let action = if repeat { KeyAction::Repeat } else { KeyAction::Press };
        self.send_input(ScreenInput::Key { code, action, mods, chord });
        true
    }

    /// The key a ⌘ or ⌃ chord at `at` goes as, and the character it names. While the worker
    /// types under its own source, not this device's, the chord names the character this
    /// device's layout put on the key, and the worker presses the key that types it under its
    /// own layout, so ⌘Z on a German keyboard is undo on a US worker and ⌘A on AZERTY stays ⌘A
    /// on an AZERTY one. The key sent is the character's place on a US keyboard, where the
    /// worker presses a character its layout lacks: every Latin QWERTY layout agrees on it, and
    /// so does the ASCII layout macOS matches ⌘ against under a non-Latin one. Under this
    /// device's source, and for the keypad, the key's own place is right and names nothing.
    fn chord_code(&mut self, at: KeyCode, keystroke: &Keystroke) -> (KeyCode, Option<String>) {
        self.keyboard.chorded.retain(|&(place, _)| place != at);
        let m = keystroke.modifiers;
        if self.keyboard.applied || !(m.platform || m.control) || is_keypad(at) {
            return (at, None);
        }
        let chord = chord_char(&keystroke.key);
        let by_character = keys::key_code(&keystroke.key);
        if by_character == KeyCode::Unidentified || by_character == at {
            return (at, chord);
        }
        self.keyboard.chorded.push((at, by_character));
        (by_character, chord)
    }

    /// A key went down (or repeats) on the worker: remember it as held and send it.
    fn press_key(&mut self, code: KeyCode, repeat: bool, mods: Mods) {
        self.hold_key(code);
        let action = if repeat { KeyAction::Repeat } else { KeyAction::Press };
        self.send_input(ScreenInput::Key { code, action, mods, chord: None });
    }

    /// `code` is down on the worker until its release goes.
    fn hold_key(&mut self, code: KeyCode) {
        if !self.held.contains(&code) {
            self.held.push(code);
        }
    }

    /// A key whose press went to the worker was let go; one whose press did not is not sent.
    fn release_key(&mut self, code: KeyCode, mods: Mods) -> bool {
        let Some(at) = self.held.iter().position(|&c| c == code) else { return false };
        self.held.swap_remove(at);
        self.send_input(ScreenInput::Key { code, action: KeyAction::Release, mods, chord: None });
        true
    }

    pub(super) fn key_up(&mut self, ev: &KeyUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if self.key_released(&ev.keystroke) {
            cx.stop_propagation();
        }
    }

    /// A key went up; whether its press had gone to the worker.
    pub(super) fn key_released(&mut self, keystroke: &Keystroke) -> bool {
        self.drain_taken();
        let native = self.keyboard.platform.native().filter(|n| n.kind == KeyKind::Up);
        let (at, mods) = code_of(native, keystroke);
        let chorded = self.keyboard.chorded.iter().position(|&(place, _)| place == at);
        let code = chorded.map_or(at, |i| self.keyboard.chorded.swap_remove(i).1);
        self.release_key(code, mods)
    }

    pub(super) fn modifiers_changed(
        &mut self,
        ev: &ModifiersChangedEvent,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modifiers_moved(ev.modifiers, ev.capslock.on);
        cx.stop_propagation();
    }

    /// The modifier keys moved, or Caps Lock did: each modifier that went down or up goes as
    /// its own key, so a remote program sees ⌘ held on its own, ⌥ pressed over a menu, ⇧ held
    /// to run; Caps Lock goes as its state, never as a key; fn only rides on the other keys.
    pub(super) fn modifiers_moved(&mut self, now: Modifiers, caps: bool) {
        self.drain_taken();
        self.set_caps(caps);
        let was = std::mem::replace(&mut self.modifiers, now);
        let native = self.keyboard.platform.native().filter(|n| n.kind == KeyKind::Flags);
        match native.and_then(|n| KeyCode::from_mac_vk(n.vk).map(|code| (code, n.mods))) {
            // The key that moved, with its side: a modifier key toggles on each change. A press
            // needs its modifier held now, so a change GPUI reports with an older event behind
            // it (the window taking the keys) presses nothing.
            Some((code, mods)) => {
                if !matches!(code, KeyCode::CapsLock | KeyCode::Fn)
                    && !self.release_key(code, mods)
                    && holds(now, code)
                {
                    self.press_key(code, false, mods);
                }
                // GPUI reports no change that leaves the modifiers as they were, so with both
                // ⇧ keys down, letting go of the left one is never heard: whatever holds a
                // modifier that is off now is let go.
                let stale: Vec<KeyCode> = self
                    .held
                    .iter()
                    .copied()
                    .filter(|&held| is_side(held) && !holds(now, held))
                    .collect();
                for held in stale {
                    self.release_key(held, mods);
                }
            }
            // No position to read (an iPad, a test): the left key stands for both.
            None => {
                for (code, action) in modifier_keys(was, now) {
                    let mods = keys::screen_mods(now);
                    match action {
                        KeyAction::Release => {
                            self.release_key(code, mods);
                        }
                        KeyAction::Press | KeyAction::Repeat => self.press_key(code, false, mods),
                    }
                }
            }
        }
    }

    /// Press and release one key on the worker: the phone key bar and the soft keyboard, which
    /// have no key-up of their own. Armed modifiers apply and clear. A key with no place on a
    /// keyboard types its character.
    pub fn press(&mut self, mut keystroke: Keystroke, cx: &mut Context<Self>) {
        self.drain_taken();
        let armed = std::mem::take(&mut self.sticky);
        keystroke.modifiers.control |= armed.control;
        keystroke.modifiers.platform |= armed.platform;
        let paste = is_paste_chord(&keystroke);
        if paste {
            self.push_clipboard(cx);
        }
        let code = keys::key_code(&keystroke.key);
        let mods = keys::screen_mods(keystroke.modifiers);
        if paste && code != KeyCode::Unidentified {
            self.send_input(ScreenInput::PasteChord { code, mods });
            self.send_input(ScreenInput::Key {
                code,
                action: KeyAction::Release,
                mods,
                chord: None,
            });
        } else if code == KeyCode::Unidentified {
            let text = keystroke.key_char.filter(|t| !t.is_empty());
            if let Some(text) = text.filter(|_| !mods.intersects(Mods::CTRL | Mods::SUPER)) {
                self.send_input(ScreenInput::Text { text });
            }
        } else {
            // The key bar's chord names its character as a keyboard's does, while the worker
            // types under its own source.
            let chorded = keystroke.modifiers.platform || keystroke.modifiers.control;
            let chord = chord_char(&keystroke.key).filter(|_| chorded && !self.keyboard.applied);
            self.send_input(ScreenInput::Key { code, action: KeyAction::Press, mods, chord });
            self.send_input(ScreenInput::Key {
                code,
                action: KeyAction::Release,
                mods,
                chord: None,
            });
        }
        cx.notify();
    }

    /// Text committed here (an input method's or a dead key's result, dictation, the soft
    /// keyboard, "Type the clipboard"): typed on the worker as it stands, a line break as ↩ and
    /// a tab as ⇥. A single character with an armed modifier is that chord instead.
    pub(super) fn commit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.drain_taken();
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && (self.sticky.control || self.sticky.platform)
        {
            let key = c.to_lowercase().to_string();
            self.press(Keystroke { modifiers: Modifiers::default(), key, key_char: None }, cx);
            return;
        }
        let mut run = String::new();
        for c in text.chars() {
            let key = match c {
                '\n' | '\r' => KeyCode::Enter,
                '\t' => KeyCode::Tab,
                _ => {
                    run.push(c);
                    continue;
                }
            };
            if !run.is_empty() {
                self.send_input(ScreenInput::Text { text: std::mem::take(&mut run) });
            }
            let mods = Mods::empty();
            self.send_input(ScreenInput::Key {
                code: key,
                action: KeyAction::Press,
                mods,
                chord: None,
            });
            self.send_input(ScreenInput::Key {
                code: key,
                action: KeyAction::Release,
                mods,
                chord: None,
            });
        }
        if !run.is_empty() {
            self.send_input(ScreenInput::Text { text: run });
        }
    }

    /// A system shortcut's key, or a media key, taken off this Mac for the worker: pressed (or
    /// repeated), or let go. Its modifiers went already, as the person pressed them; a release
    /// whose press never went here is dropped, and one still held is let go with the rest when
    /// the tile loses the keyboard.
    pub fn system_key(&mut self, code: KeyCode, down: bool, mods: Mods, cx: &mut Context<Self>) {
        self.drain_taken();
        if let Some(key) = media_key(code) {
            self.send_input(ScreenInput::Media { key, down });
        } else if down {
            self.press_key(code, false, mods);
        } else {
            self.release_key(code, mods);
        }
        cx.notify();
    }
}

/// A modifier key with a side: ⇧, ⌃, ⌥ or ⌘, left or right.
const fn is_side(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::AltLeft
            | KeyCode::AltRight
            | KeyCode::MetaLeft
            | KeyCode::MetaRight
    )
}

/// Whether `mods` has the modifier the key `code` holds.
const fn holds(mods: Modifiers, code: KeyCode) -> bool {
    match code {
        KeyCode::ShiftLeft | KeyCode::ShiftRight => mods.shift,
        KeyCode::ControlLeft | KeyCode::ControlRight => mods.control,
        KeyCode::AltLeft | KeyCode::AltRight => mods.alt,
        KeyCode::MetaLeft | KeyCode::MetaRight => mods.platform,
        _ => false,
    }
}

/// The modifier keys that went down or up between `was` and `now`, as presses and releases.
/// The fn key is left out: the worker's own fn setting (emoji picker, dictation) would fire.
pub(super) fn modifier_keys(was: Modifiers, now: Modifiers) -> Vec<(KeyCode, KeyAction)> {
    [
        (was.shift, now.shift, KeyCode::ShiftLeft),
        (was.control, now.control, KeyCode::ControlLeft),
        (was.alt, now.alt, KeyCode::AltLeft),
        (was.platform, now.platform, KeyCode::MetaLeft),
    ]
    .into_iter()
    .filter(|(before, after, _)| before != after)
    .map(|(_, down, code)| (code, if down { KeyAction::Press } else { KeyAction::Release }))
    .collect()
}

#[cfg(test)]
mod tests;

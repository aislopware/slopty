use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{EntityInputHandler as _, Keystroke, Modifiers};
use slopty_client::ScreenHandle;
use slopty_core::{DisplayId, StreamId};
use slopty_platform::keyboard::{KeyKind, NativeKey, Taken};
use slopty_proto::ClientMsg;
use slopty_proto::input::{KeyAction, KeyCode, Mods};
use slopty_proto::screen::{CaptureTarget, MediaKey, Quality, ScreenInput, ScreenRequest};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use super::KeyPlatform;
use crate::screen::{Opened, ScreenView};

const US: &str = "com.apple.keylayout.US";
const FRENCH: &str = "com.apple.keylayout.French";
const TELEX: &str = "com.apple.inputmethod.VietnameseIM.VietnameseSimpleTelex";

/// A keyboard the test plays: the event being dispatched, the input source, Caps Lock, and
/// the keys the monitor would take.
#[derive(Default)]
struct Fake {
    native: Cell<Option<NativeKey>>,
    source: RefCell<Option<String>>,
    caps: Cell<Option<bool>>,
    taken: RefCell<Option<Taken>>,
    switched: RefCell<Option<Box<dyn Fn()>>>,
}

impl KeyPlatform for Fake {
    fn native(&self) -> Option<NativeKey> {
        self.native.get()
    }

    fn source(&self) -> Option<String> {
        self.source.borrow().clone()
    }

    fn caps_lock(&self) -> Option<bool> {
        self.caps.get()
    }

    fn take(&self, wake: Box<dyn Fn()>) -> Option<Taken> {
        let taken = Taken::detached(wake);
        self.taken.replace(Some(taken.clone()));
        Some(taken)
    }

    fn watch(&self, changed: Box<dyn Fn()>) -> Option<Box<dyn std::any::Any>> {
        self.switched.replace(Some(changed));
        Some(Box::new(()))
    }
}

impl Fake {
    fn on(source: &str) -> Rc<Self> {
        let fake = Rc::new(Self::default());
        fake.source.replace(Some(source.to_owned()));
        fake.caps.set(Some(false));
        fake
    }

    /// The monitor offers a key: whether it took it.
    fn offer(&self, vk: u16, kind: KeyKind, mods: Mods) -> bool {
        self.taken.borrow().as_ref().is_some_and(|t| t.offer(NativeKey { vk, kind, mods }))
    }

    /// The person switches input source.
    fn switch(&self, source: &str) {
        self.source.replace(Some(source.to_owned()));
        if let Some(changed) = self.switched.borrow().as_ref() {
            changed();
        }
    }
}

/// A focused view in a window, on `fake`'s keyboard, with what it sends.
fn focused<'a>(
    cx: &'a mut gpui::TestAppContext,
    fake: &Rc<Fake>,
) -> (gpui::Entity<ScreenView>, mpsc::Receiver<ClientMsg>, &'a mut gpui::VisualTestContext) {
    let (out, rx) = mpsc::channel(256);
    let opened = Opened {
        stream: StreamId(4),
        target: CaptureTarget::Display(DisplayId(2)),
        size: (800, 600),
        quality: Quality { scale: 1.0, ..Quality::default() },
    };
    let platform: Rc<dyn KeyPlatform> = Rc::<Fake>::clone(fake);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut view =
            ScreenView::new(opened, ScreenHandle::detached(StreamId(4)), out, Theme::default(), cx);
        view.set_key_platform(platform, cx);
        window.focus(&view.focus, cx);
        view
    });
    cx.run_until_parked();
    view.update(cx, |v, _| v.keyboard_focused());
    (view, rx, cx)
}

/// The input sent to the worker since the last look.
fn inputs(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<ScreenInput> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|msg| match msg {
            ClientMsg::Screen(ScreenRequest::Input { input, .. }) => Some(input),
            _ => None,
        })
        .collect()
}

fn key(code: KeyCode, action: KeyAction, mods: Mods) -> ScreenInput {
    ScreenInput::Key { code, action, mods }
}

fn text(t: &str) -> ScreenInput {
    ScreenInput::Text { text: t.to_owned() }
}

fn stroke(key: &str, modifiers: Modifiers, key_char: Option<&str>) -> Keystroke {
    Keystroke { modifiers, key: key.to_owned(), key_char: key_char.map(str::to_owned) }
}

/// This Mac's text system commits `text` in one go, as an input method does.
fn commit(view: &gpui::Entity<ScreenView>, cx: &mut gpui::VisualTestContext, text: &str) {
    view.update_in(cx, |v, window, cx| v.replace_text_in_range(None, text, window, cx));
}

/// The worker answers that it took `source`.
fn took(view: &gpui::Entity<ScreenView>, cx: &mut gpui::VisualTestContext, source: &str) {
    view.update(cx, |v, _| v.set_keyboard_source(source, true));
}

/// Stage 0's pin (`design-input-fidelity.md` §1.1): a character outside ASCII committed here
/// reaches the worker. Before, it became a key named `Unidentified` that the worker dropped.
#[gpui::test]
fn a_non_ascii_character_reaches_the_worker(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    let (_view, mut rx, cx) = focused(cx, &fake);
    inputs(&mut rx);
    for typed in ["é", "ü", "ж", "ế"] {
        cx.simulate_input(typed);
        assert_eq!(inputs(&mut rx), [text(typed)], "{typed}");
    }
}

/// While the worker has not taken this Mac's source, a dead key composes here: ⌥E goes to
/// the text system (the view leaves it), its accent is marked, nothing is sent, and the
/// result goes as text once the next key commits it.
#[gpui::test]
fn a_dead_key_composes_here_until_the_worker_takes_the_source(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    let (view, mut rx, cx) = focused(cx, &fake);
    inputs(&mut rx);
    let alt = Modifiers { alt: true, ..Modifiers::default() };
    let taken = view.update(cx, |v, cx| v.key_pressed(&stroke("e", alt, None), false, cx));
    assert!(!taken, "⌥E goes on to the text system");
    view.update_in(cx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "´", None, window, cx);
    });
    assert!(inputs(&mut rx).is_empty(), "marked text is not sent");
    let plain = Modifiers::default();
    let taken = view.update(cx, |v, cx| v.key_pressed(&stroke("e", plain, Some("e")), false, cx));
    assert!(!taken);
    cx.simulate_input("é");
    assert_eq!(inputs(&mut rx), [text("é")]);
}

/// Once the worker took this Mac's source, the dead key is the worker's: ⌥E and E are taken
/// off the app before the text system and go as the E key twice, ⌥ on the first, so the
/// remote app composes é itself.
#[gpui::test]
fn a_dead_key_goes_raw_once_the_worker_took_the_source(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    let (view, mut rx, cx) = focused(cx, &fake);
    assert_eq!(
        inputs(&mut rx),
        [ScreenInput::KeyboardSource { source: US.to_owned() }, ScreenInput::Lock { caps: false }]
    );
    assert!(!fake.offer(0x0e, KeyKind::Down, Mods::ALT), "nothing taken before the answer");
    took(&view, cx, US);
    assert!(view.read_with(cx, |v, _| v.worker_composes()));
    let e = 0x0e;
    assert!(fake.offer(e, KeyKind::Down, Mods::ALT));
    assert!(fake.offer(e, KeyKind::Up, Mods::ALT));
    assert!(fake.offer(e, KeyKind::Down, Mods::empty()));
    assert!(fake.offer(e, KeyKind::Up, Mods::empty()));
    cx.run_until_parked();
    assert_eq!(
        inputs(&mut rx),
        [
            key(KeyCode::E, KeyAction::Press, Mods::ALT),
            key(KeyCode::E, KeyAction::Release, Mods::ALT),
            key(KeyCode::E, KeyAction::Press, Mods::empty()),
            key(KeyCode::E, KeyAction::Release, Mods::empty()),
        ]
    );
}

/// Vietnamese typed with Telex, and with VNI, on this Mac's input method while the worker
/// lacks it: the marked stages stay here and only each committed word goes, as text.
#[gpui::test]
fn a_vietnamese_ime_commit_goes_as_text(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(TELEX);
    let (view, mut rx, cx) = focused(cx, &fake);
    view.update(cx, |v, _| v.set_keyboard_source(TELEX, false));
    inputs(&mut rx);
    let mark = |view: &gpui::Entity<ScreenView>, cx: &mut gpui::VisualTestContext, m: &str| {
        view.update_in(cx, |v, window, cx| {
            v.replace_and_mark_text_in_range(None, m, None, window, cx);
        });
    };
    // Telex: t i e e n g s → tiếng; VNI: V i e 6 5 t → Việt.
    for (keys, marks, word) in [
        ("tieengs", ["t", "ti", "tie", "tiê", "tiên", "tiêng", "tiếng"].as_slice(), "tiếng"),
        ("Vie65t", ["V", "Vi", "Vie", "Viê", "Việ", "Việt"].as_slice(), "Việt"),
    ] {
        for (c, m) in keys.chars().zip(marks.iter().chain(std::iter::repeat(&"")).copied()) {
            let s = c.to_string();
            let left = view.update(cx, |v, cx| {
                !v.key_pressed(&stroke(&s, Modifiers::default(), Some(&s)), false, cx)
            });
            assert!(left, "{c} is the input method's");
            if !m.is_empty() {
                mark(&view, cx, m);
            }
        }
        assert!(inputs(&mut rx).is_empty(), "nothing goes while {word} is marked");
        commit(&view, cx, word);
        assert_eq!(inputs(&mut rx), [text(word)]);
    }
}

/// A Japanese input method's commit goes whole, as text; its candidates' keys (Space,
/// arrows while marked) are the input method's.
#[gpui::test]
fn a_japanese_ime_commit_goes_as_text(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on("com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese");
    let (view, mut rx, cx) = focused(cx, &fake);
    inputs(&mut rx);
    view.update_in(cx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "にほんご", None, window, cx);
    });
    let space = stroke("space", Modifiers::default(), Some(" "));
    assert!(!view.update(cx, |v, cx| v.key_pressed(&space, false, cx)), "Space picks a candidate");
    assert!(inputs(&mut rx).is_empty());
    commit(&view, cx, "日本語");
    assert_eq!(inputs(&mut rx), [text("日本語")]);
}

/// ⌘Q on an AZERTY Mac is the key labelled A, at the Q key's place: the worker gets the place
/// (`KeyCode::Q`, `kVK_ANSI_Q`), which its AZERTY layout reads as ⌘A, as the person meant.
/// Before, the character "a" went as the US A key, which AZERTY reads as ⌘Q: the app quit.
#[gpui::test]
fn a_key_goes_by_position_not_by_character(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(FRENCH);
    let (view, mut rx, cx) = focused(cx, &fake);
    took(&view, cx, FRENCH);
    inputs(&mut rx);
    let cmd = Modifiers { platform: true, ..Modifiers::default() };
    fake.native.set(Some(NativeKey { vk: 0x0c, kind: KeyKind::Down, mods: Mods::SUPER }));
    assert!(view.update(cx, |v, cx| v.key_pressed(&stroke("a", cmd, None), false, cx)));
    fake.native.set(Some(NativeKey { vk: 0x0c, kind: KeyKind::Up, mods: Mods::SUPER }));
    assert!(view.update(cx, |v, _| v.key_released(&stroke("a", cmd, None))));
    assert_eq!(
        inputs(&mut rx),
        [
            key(KeyCode::Q, KeyAction::Press, Mods::SUPER),
            key(KeyCode::Q, KeyAction::Release, Mods::SUPER)
        ]
    );
    // ⌘⌥⎋ and ⌃⌘Q stay on this Mac.
    let force = Modifiers { platform: true, alt: true, ..Modifiers::default() };
    assert!(!view.update(cx, |v, cx| v.key_pressed(&stroke("escape", force, None), false, cx)));
    let lock = Modifiers { platform: true, control: true, ..Modifiers::default() };
    assert!(!view.update(cx, |v, cx| v.key_pressed(&stroke("q", lock, None), false, cx)));
    assert!(inputs(&mut rx).is_empty());
}

/// A key the monitor took goes before a chord that came through GPUI after it, and before a
/// click: nothing overtakes a taken key.
#[gpui::test]
fn taken_keys_go_before_what_follows_them(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    let (view, mut rx, cx) = focused(cx, &fake);
    took(&view, cx, US);
    inputs(&mut rx);
    assert!(fake.offer(0x00, KeyKind::Down, Mods::empty()));
    let cmd = Modifiers { platform: true, ..Modifiers::default() };
    fake.native.set(Some(NativeKey { vk: 0x01, kind: KeyKind::Down, mods: Mods::SUPER }));
    view.update(cx, |v, cx| v.key_pressed(&stroke("s", cmd, None), false, cx));
    assert_eq!(
        inputs(&mut rx),
        [
            key(KeyCode::A, KeyAction::Press, Mods::empty()),
            key(KeyCode::S, KeyAction::Press, Mods::SUPER)
        ]
    );
}

/// A right-hand modifier goes as its own key with its side, down and up.
#[gpui::test]
fn right_modifiers_keep_their_side(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    let (view, mut rx, cx) = focused(cx, &fake);
    inputs(&mut rx);
    let shift = Modifiers { shift: true, ..Modifiers::default() };
    let right = Mods::SHIFT | Mods::SHIFT_RIGHT;
    fake.native.set(Some(NativeKey { vk: 0x3c, kind: KeyKind::Flags, mods: right }));
    view.update(cx, |v, _| v.modifiers_moved(shift, false));
    fake.native.set(Some(NativeKey { vk: 0x3c, kind: KeyKind::Flags, mods: Mods::empty() }));
    view.update(cx, |v, _| v.modifiers_moved(Modifiers::default(), false));
    assert_eq!(
        inputs(&mut rx),
        [
            key(KeyCode::ShiftRight, KeyAction::Press, right),
            key(KeyCode::ShiftRight, KeyAction::Release, Mods::empty()),
        ]
    );
}

/// Caps Lock goes as its state when the tile takes the keyboard and whenever it changes,
/// never as a key, even when the event being handled is its key.
#[gpui::test]
fn caps_lock_state_follows_focus_and_changes(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    fake.caps.set(Some(true));
    let (view, mut rx, cx) = focused(cx, &fake);
    assert!(inputs(&mut rx).contains(&ScreenInput::Lock { caps: true }), "on focus");
    fake.native.set(Some(NativeKey { vk: 0x39, kind: KeyKind::Flags, mods: Mods::empty() }));
    view.update(cx, |v, _| v.modifiers_moved(Modifiers::default(), false));
    assert_eq!(inputs(&mut rx), [ScreenInput::Lock { caps: false }], "no key");
    view.update(cx, |v, _| v.modifiers_moved(Modifiers::default(), false));
    assert!(inputs(&mut rx).is_empty(), "unchanged: nothing");
}

/// The worker hears this Mac's input source when the tile takes the keyboard and again when
/// the person switches; until it answers for the new one, this Mac composes and nothing is
/// taken; an answer for a source since replaced changes nothing.
#[gpui::test]
fn the_source_is_told_on_focus_and_on_every_switch(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    let (view, mut rx, cx) = focused(cx, &fake);
    inputs(&mut rx);
    took(&view, cx, US);
    assert!(view.read_with(cx, |v, _| v.worker_composes()));
    fake.switch(TELEX);
    cx.run_until_parked();
    assert_eq!(inputs(&mut rx), [ScreenInput::KeyboardSource { source: TELEX.to_owned() }]);
    assert!(!view.read_with(cx, |v, _| v.worker_composes()), "composes here meanwhile");
    took(&view, cx, US);
    assert!(!view.read_with(cx, |v, _| v.worker_composes()), "a stale answer");
    took(&view, cx, TELEX);
    assert!(view.read_with(cx, |v, _| v.worker_composes()));
    view.update(cx, |v, cx| v.keyboard_blurred(cx));
    assert!(!view.read_with(cx, |v, _| v.worker_composes()), "nothing taken without the focus");
}

/// "Type the clipboard" with text outside ASCII types it all: runs as text, a line break as ↩.
#[gpui::test]
fn typed_text_outside_ascii_goes_whole(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    let (view, mut rx, cx) = focused(cx, &fake);
    inputs(&mut rx);
    view.update(cx, |v, cx| v.commit_text("Xin chào\nthế giới", cx));
    assert_eq!(
        inputs(&mut rx),
        [
            text("Xin chào"),
            key(KeyCode::Enter, KeyAction::Press, Mods::empty()),
            key(KeyCode::Enter, KeyAction::Release, Mods::empty()),
            text("thế giới"),
        ]
    );
}

/// Play/pause and the track keys taken off this Mac go to the worker as media keys.
#[gpui::test]
fn media_keys_go_to_the_worker(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    let (view, mut rx, cx) = focused(cx, &fake);
    inputs(&mut rx);
    view.update(cx, |v, cx| {
        v.system_key(KeyCode::MediaPlayPause, true, Mods::empty(), cx);
        v.system_key(KeyCode::MediaPlayPause, false, Mods::empty(), cx);
    });
    assert_eq!(
        inputs(&mut rx),
        [
            ScreenInput::Media { key: MediaKey::PlayPause, down: true },
            ScreenInput::Media { key: MediaKey::PlayPause, down: false },
        ]
    );
}

/// A keypad key goes as the keypad's own key, not the main row's, whether GPUI names it `5`
/// (read by its place) or `kp5`, and even while this Mac composes.
#[gpui::test]
fn keypad_keys_keep_their_place(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    let (view, mut rx, cx) = focused(cx, &fake);
    inputs(&mut rx);
    let plain = Modifiers::default();
    fake.native.set(Some(NativeKey { vk: 0x57, kind: KeyKind::Down, mods: Mods::empty() }));
    assert!(view.update(cx, |v, cx| v.key_pressed(&stroke("5", plain, Some("5")), false, cx)));
    fake.native.set(None);
    assert!(view.update(cx, |v, cx| v.key_pressed(&stroke("kpenter", plain, None), false, cx)));
    assert_eq!(
        inputs(&mut rx),
        [
            key(KeyCode::Numpad5, KeyAction::Press, Mods::empty()),
            key(KeyCode::NumpadEnter, KeyAction::Press, Mods::empty()),
        ]
    );
}

/// With both ⇧ keys down, GPUI never reports the left one going up (the modifiers stay as
/// they were), so the release of the right one lets go of the left too: nothing stays down on
/// the worker.
#[gpui::test]
fn overlapping_modifiers_are_all_let_go(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    let (view, mut rx, cx) = focused(cx, &fake);
    inputs(&mut rx);
    let shift = Modifiers { shift: true, ..Modifiers::default() };
    fake.native.set(Some(NativeKey { vk: 0x38, kind: KeyKind::Flags, mods: Mods::SHIFT }));
    view.update(cx, |v, _| v.modifiers_moved(shift, false));
    // R⇧ down and L⇧ up leave ⇧ held: GPUI drops both.
    fake.native.set(Some(NativeKey { vk: 0x3c, kind: KeyKind::Flags, mods: Mods::empty() }));
    view.update(cx, |v, _| v.modifiers_moved(Modifiers::default(), false));
    assert_eq!(
        inputs(&mut rx),
        [
            key(KeyCode::ShiftLeft, KeyAction::Press, Mods::SHIFT),
            key(KeyCode::ShiftLeft, KeyAction::Release, Mods::empty()),
        ]
    );
}

/// Telex typed straight after the tile takes the keyboard: the worker's answer lands while
/// "tiê" is marked here. The word finishes here and goes as text, and only then are the keys
/// taken, so none is lost to a stranded composition or goes ahead of the word.
#[gpui::test]
fn a_composition_under_way_when_the_worker_answers_finishes_here(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(TELEX);
    let (view, mut rx, cx) = focused(cx, &fake);
    inputs(&mut rx);
    let typed = |view: &gpui::Entity<ScreenView>, cx: &mut gpui::VisualTestContext, c: &str, m| {
        let left = view.update(cx, |v, cx| {
            !v.key_pressed(&stroke(c, Modifiers::default(), Some(c)), false, cx)
        });
        assert!(left, "{c} is the input method's");
        view.update_in(cx, |v, window, cx| {
            v.replace_and_mark_text_in_range(None, m, None, window, cx);
        });
    };
    for (c, m) in [("t", "t"), ("i", "ti"), ("e", "tie"), ("e", "tiê")] {
        typed(&view, cx, c, m);
    }
    took(&view, cx, TELEX);
    assert!(!view.read_with(cx, |v, _| v.worker_composes()), "not while a word is marked");
    assert!(!fake.offer(0x2d, KeyKind::Down, Mods::empty()), "n goes to the input method");
    for (c, m) in [("n", "tiên"), ("g", "tiêng"), ("s", "tiếng")] {
        typed(&view, cx, c, m);
    }
    commit(&view, cx, "tiếng");
    assert!(view.read_with(cx, |v, _| v.worker_composes()), "taken once the word is in");
    assert!(fake.offer(0x11, KeyKind::Down, Mods::empty()), "the next key is taken");
    cx.run_until_parked();
    assert_eq!(inputs(&mut rx), [text("tiếng"), key(KeyCode::T, KeyAction::Press, Mods::empty())]);
}

/// Taking the keyboard again under the source the worker already took keeps taking the keys:
/// the first ones typed go raw at once, and the worker hears the source again.
#[gpui::test]
fn the_keyboard_taken_again_under_the_same_source_takes_keys_at_once(
    cx: &mut gpui::TestAppContext,
) {
    let fake = Fake::on(US);
    let (view, mut rx, cx) = focused(cx, &fake);
    took(&view, cx, US);
    view.update(cx, |v, cx| v.keyboard_blurred(cx));
    inputs(&mut rx);
    view.update(cx, |v, _| v.keyboard_focused());
    assert!(view.read_with(cx, |v, _| v.worker_composes()), "no wait for the answer");
    assert!(fake.offer(0x00, KeyKind::Down, Mods::empty()));
    cx.run_until_parked();
    assert_eq!(
        inputs(&mut rx),
        [
            ScreenInput::KeyboardSource { source: US.to_owned() },
            ScreenInput::Lock { caps: false },
            key(KeyCode::A, KeyAction::Press, Mods::empty()),
        ]
    );
}

/// The worker saying another client took its source sends this Mac back to composing.
#[gpui::test]
fn a_source_the_worker_lost_is_composed_here_again(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(US);
    let (view, _rx, cx) = focused(cx, &fake);
    took(&view, cx, US);
    assert!(view.read_with(cx, |v, _| v.worker_composes()));
    view.update(cx, |v, _| v.set_keyboard_source(US, false));
    assert!(!view.read_with(cx, |v, _| v.worker_composes()), "composed here, nothing taken");
}

/// While the worker types under its own source, a ⌘ chord goes by its character's place on a
/// US keyboard: ⌘A typed on AZERTY (the Q key's place) goes as ⌘A, and its release lets the
/// same key go. By place it went as ⌘Q, and the worker's app quit.
#[gpui::test]
fn a_chord_the_worker_reads_under_its_own_source_goes_by_character(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(FRENCH);
    let (view, mut rx, cx) = focused(cx, &fake);
    view.update(cx, |v, _| v.set_keyboard_source(FRENCH, false));
    inputs(&mut rx);
    let cmd = Modifiers { platform: true, ..Modifiers::default() };
    fake.native.set(Some(NativeKey { vk: 0x0c, kind: KeyKind::Down, mods: Mods::SUPER }));
    assert!(view.update(cx, |v, cx| v.key_pressed(&stroke("a", cmd, None), false, cx)));
    fake.native.set(Some(NativeKey { vk: 0x0c, kind: KeyKind::Up, mods: Mods::SUPER }));
    assert!(view.update(cx, |v, _| v.key_released(&stroke("a", cmd, None))));
    // An arrow is its place under any source.
    fake.native.set(Some(NativeKey { vk: 0x7b, kind: KeyKind::Down, mods: Mods::SUPER }));
    assert!(view.update(cx, |v, cx| v.key_pressed(&stroke("left", cmd, None), false, cx)));
    assert_eq!(
        inputs(&mut rx),
        [
            key(KeyCode::A, KeyAction::Press, Mods::SUPER),
            key(KeyCode::A, KeyAction::Release, Mods::SUPER),
            key(KeyCode::ArrowLeft, KeyAction::Press, Mods::SUPER),
        ]
    );
}

/// A tile without the keyboard for a while lets the worker's source go; one that takes the
/// keyboard back sooner keeps its claim, so hopping between tiles costs no switch.
#[gpui::test]
fn a_tile_left_a_while_lets_the_source_go(cx: &mut gpui::TestAppContext) {
    let fake = Fake::on(FRENCH);
    let (view, mut rx, cx) = focused(cx, &fake);
    took(&view, cx, FRENCH);
    let half = super::RELEASE_AFTER.checked_div(2).unwrap();
    view.update(cx, |v, cx| v.keyboard_blurred(cx));
    cx.executor().advance_clock(half);
    view.update(cx, |v, _| v.keyboard_focused());
    view.update(cx, |v, cx| v.keyboard_blurred(cx));
    cx.executor().advance_clock(half);
    cx.run_until_parked();
    assert!(!inputs(&mut rx).contains(&ScreenInput::KeyboardReleased), "back before the wait");
    cx.executor().advance_clock(half);
    cx.run_until_parked();
    assert_eq!(inputs(&mut rx), [ScreenInput::KeyboardReleased]);
    view.update(cx, |v, _| v.keyboard_focused());
    assert!(view.read_with(cx, |v, _| v.worker_composes()), "taken again at once");
    assert_eq!(
        inputs(&mut rx),
        [
            ScreenInput::KeyboardSource { source: FRENCH.to_owned() },
            ScreenInput::Lock { caps: false }
        ]
    );
}

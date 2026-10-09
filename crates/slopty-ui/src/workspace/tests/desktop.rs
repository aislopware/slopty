//! A remote window or desktop tile beyond its header, in the headless workspace: the clipboard
//! typed into it, a display made for this device that follows its tile, and the system's
//! shortcuts sent to a remote Mac through a stand-in for the session tap (a test never makes a
//! tap, so it never asks for Accessibility).

use std::cell::RefCell;
use std::rc::Rc;

use slopty_core::{DisplayId, WindowId};
use slopty_proto::input::{KeyAction, KeyCode, Mods};
use slopty_proto::screen::{DisplayKey, ScreenInput, VideoCodec, VirtualDisplay};

use super::*;
use crate::workspace::actions::ToggleOwnWindow;
use crate::workspace::desktop::{
    BACK_TO_PHYSICAL, Chord, KeyPort, ONLY_CHORDS, OPEN_SIZED, SEND_SYSTEM_KEYS, TYPE_CLIPBOARD,
    Taking,
};
use crate::workspace::popout::PopOutView;

/// `target` streams as `stream` on `fake`'s worker, `width` × `height`.
fn opened(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    stream: u32,
    target: CaptureTarget,
    (width, height): (u32, u32),
) {
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        let event = ScreenEvent::Opened {
            stream: StreamId(stream),
            target,
            codec: VideoCodec::Hevc,
            width,
            height,
            scale: 2.0,
            stripes: Vec::new(),
        };
        v.screen_event(key, event, cx);
    });
    cx.run_until_parked();
}

/// Everything the workspace sent, the outboxes emptied as the channel frees and paced typing
/// given its frames.
fn sent(fake: &mut Fake, cx: &VisualTestContext) -> Vec<ClientMsg> {
    let mut all = Vec::new();
    let mut quiet = 0_u8;
    while quiet < 3 {
        cx.run_until_parked();
        let more = fake.drain();
        quiet = if more.is_empty() { quiet.saturating_add(1) } else { 0 };
        all.extend(more);
        cx.executor().advance_clock(Duration::from_millis(16));
    }
    all
}

/// What the worker was sent to type: text as it stands, a key press by its code.
fn typed(msgs: &[ClientMsg]) -> Vec<String> {
    msgs.iter()
        .filter_map(|m| match m {
            ClientMsg::Screen(ScreenRequest::Input { input, .. }) => match input {
                ScreenInput::Text { text } => Some(text.clone()),
                ScreenInput::Key { code, action: KeyAction::Press, .. } => {
                    Some(format!("{code:?}"))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn palette_has(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, label: &str) -> bool {
    view.update(cx, |v, cx| v.palette_lines(cx).iter().any(|l| l.label == label))
}

/// "Type the clipboard" types this device's clipboard into the focused window as text, a line
/// break as one ↩; a clipboard past 1 KB is cut there and says so, and one with no text
/// types nothing and says that.
#[gpui::test]
fn the_clipboard_is_typed_into_the_focused_window(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let window = WindowId(7);
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window }, 1);
    assert!(!palette_has(&view, cx, TYPE_CLIPBOARD), "not before a window has the focus");
    opened(&view, cx, &fake, 1, CaptureTarget::Window(window), (1280, 800));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    assert!(palette_has(&view, cx, TYPE_CLIPBOARD));
    sent(&mut fake, cx);

    let type_it = |cx: &mut VisualTestContext| {
        view.update_in(cx, |v, window, cx| v.type_clipboard(&TypeClipboard, window, cx));
    };
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("ok\r\nA".to_owned()));
    type_it(cx);
    assert_eq!(typed(&sent(&mut fake, cx)), ["ok", "Enter", "A"]);

    cx.write_to_clipboard(gpui::ClipboardItem::new_string("é".repeat(600)));
    type_it(cx);
    let all = typed(&sent(&mut fake, cx)).concat();
    assert_eq!(all, "é".repeat(512), "1 KB of two-byte characters, none cut in half");
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("Typed the first 1 KB of the clipboard"));

    cx.write_to_clipboard(gpui::ClipboardItem::new_string(String::new()));
    type_it(cx);
    assert_eq!(typed(&sent(&mut fake, cx)), Vec::<String>::new());
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("The clipboard holds no text"));
}

/// The body of `tile` in device pixels, as the workspace sizes a display to it.
fn body_pixels(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    tile: TileRef,
) -> (u32, u32) {
    let scale = cx.update(|window, _| window.scale_factor());
    view.read_with(cx, |v, _| {
        let placed = v.placed_tiles().into_iter().find(|p| p.tile == tile).expect("placed");
        // None for a tab's one tile, whose header is the title bar's.
        let header = v.header_h();
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
        let px = |points: f32| (points * scale).round() as u32;
        (px(placed.rect.w), px(placed.rect.h - header))
    })
}

/// On a worker that can make one, a display tile is streamed from a display made for this
/// device at the tile's size and the screen's scale, in place of the physical one; the
/// display takes the tile's new size once it has held still, and the command again goes back
/// to the physical display. A worker that cannot is never offered it.
#[gpui::test]
fn a_display_made_for_this_device_follows_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let dir = tempfile::tempdir().expect("a directory");
    view.update(cx, |v, _| v.set_layout_path(dir.path().join("layout.json")));
    let mut fake = connect(&view, cx, 1, "studio");
    let physical = DisplayId(1);
    let _shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let tile = arrives(&view, cx, &fake, ItemKind::Display { display: physical }, 2);
    opened(&view, cx, &fake, 1, CaptureTarget::Display(physical), (2560, 1440));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    assert!(!palette_has(&view, cx, OPEN_SIZED), "this worker makes no displays");

    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(key, WorkerCaps { virtual_displays: true, ..healthy() }, cx);
    });
    cx.run_until_parked();
    assert!(palette_has(&view, cx, OPEN_SIZED));
    sent(&mut fake, cx);
    let keyboard = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            let screen = view.read(cx).screen(tile.item).cloned();
            screen.is_some_and(|s| s.read(cx).focus_handle(cx).is_focused(window))
        })
    };
    cx.update(|window, cx| {
        let screen = view.read(cx).screen(tile.item).cloned().expect("streaming");
        window.focus(&screen.read(cx).focus_handle(cx), cx);
    });

    let toggle = |cx: &mut VisualTestContext| {
        view.update_in(cx, |v, window, cx| {
            v.toggle_sized_display(&ToggleSizedDisplay, window, cx);
        });
    };
    toggle(cx);
    let asked = sent(&mut fake, cx);
    let body = body_pixels(&view, cx, tile);
    let scale = cx.update(|window, _| window.scale_factor());
    let display_key: DisplayKey = asked
        .iter()
        .find_map(|m| match m {
            ClientMsg::Screen(ScreenRequest::OpenDisplay { key, shape, .. }) => {
                assert_eq!((shape.width, shape.height), body, "sized to the tile's body");
                assert!((shape.scale - scale).abs() < f32::EPSILON, "at the screen's scale");
                Some(*key)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("a display asked for: {asked:?}"));
    assert!(
        asked.iter().any(|m| matches!(m, ClientMsg::Screen(ScreenRequest::Close(StreamId(1))))),
        "the physical display's stream goes: {asked:?}"
    );
    assert!(dir.path().join(slopty_client::screen::display::KEY_FILE).exists(), "key kept");
    let saved = view.read_with(cx, |v, _| v.to_save().displays);
    let kept = slopty_client::layout::SavedDisplay { tile, sized: true };
    assert_eq!(saved, [kept], "on a Mac, the pick is kept for the next run");

    let made = DisplayId(9);
    view.update_in(cx, |v, _w, cx| {
        let told = ScreenEvent::Display {
            stream: StreamId(2),
            key: display_key,
            display: VirtualDisplay::Made(made),
        };
        v.screen_event(key, told, cx);
    });
    opened(&view, cx, &fake, 2, CaptureTarget::Display(made), body);
    let stream = view.read_with(cx, |v, cx| v.screen(tile.item).map(|s| s.read(cx).stream()));
    assert_eq!(stream, Some(StreamId(2)), "the tile shows the made display");
    assert!(keyboard(cx), "and its view has the keyboard the physical one had");
    view.update_in(cx, |v, window, cx| window.focus(&v.focus, cx));
    assert!(palette_has(&view, cx, BACK_TO_PHYSICAL));
    sent(&mut fake, cx);

    // Zoomed, the docked navigator steps aside and the pane takes its room.
    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
    let wider = body_pixels(&view, cx, tile);
    assert_ne!(wider, body, "the pane changed width");
    let resized = |msgs: &[ClientMsg]| {
        msgs.iter()
            .filter_map(|m| match m {
                ClientMsg::Screen(ScreenRequest::Resize { stream, width, height, scale }) => {
                    Some((*stream, (*width, *height), *scale))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert!(resized(&sent(&mut fake, cx)).is_empty(), "not while it may still move");
    cx.executor().advance_clock(slopty_client::screen::display::SETTLE);
    cx.run_until_parked();
    assert_eq!(resized(&sent(&mut fake, cx)), [(StreamId(2), wider, None)], "once it held");

    toggle(cx);
    let back = sent(&mut fake, cx);
    assert!(
        back.iter().any(|m| matches!(m, ClientMsg::Screen(ScreenRequest::Close(StreamId(2))))),
        "{back:?}"
    );
    assert!(
        back.iter().any(|m| matches!(
            m,
            ClientMsg::Screen(ScreenRequest::Open { target: CaptureTarget::Display(d), .. })
                if *d == physical
        )),
        "the physical display again: {back:?}"
    );
}

/// On a touch device a desktop comes at the device's own size unasked: the display tile opens
/// on a display made for it, never on the Mac's physical display first. Turned back to the
/// physical display, the pick is saved with the layout, and the next run starts from it; a pick
/// that matches what the device does unasked is not kept.
#[gpui::test]
fn a_touch_device_opens_a_desktop_at_its_own_size_and_keeps_the_pick(cx: &mut TestAppContext) {
    use slopty_client::layout::SavedDisplay;

    let (view, cx) = workspace(cx);
    let dir = tempfile::tempdir().expect("a directory");
    view.update(cx, |v, _| {
        v.set_layout_path(dir.path().join("layout.json"));
        v.desktop.sized_first();
    });
    let mut fake = connect(&view, cx, 1, "studio");
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(key, WorkerCaps { virtual_displays: true, ..healthy() }, cx);
    });
    let physical = DisplayId(1);
    sent(&mut fake, cx);
    let tile = arrives(&view, cx, &fake, ItemKind::Display { display: physical }, 2);
    let asked = sent(&mut fake, cx);
    let body = body_pixels(&view, cx, tile);
    let made = asked.iter().find_map(|m| match m {
        ClientMsg::Screen(ScreenRequest::OpenDisplay { shape, .. }) => {
            Some((shape.width, shape.height))
        }
        _ => None,
    });
    assert_eq!(made, Some(body), "a display made for this device, sized to the tile: {asked:?}");
    let physical_open = |msgs: &[ClientMsg]| {
        msgs.iter().any(|m| {
            matches!(
                m,
                ClientMsg::Screen(ScreenRequest::Open { target: CaptureTarget::Display(d), .. })
                    if *d == physical
            )
        })
    };
    assert!(!physical_open(&asked), "never the physical display first: {asked:?}");
    let saved = view.read_with(cx, |v, _| v.to_save().displays);
    assert!(saved.is_empty(), "what the device does unasked is not a pick to keep");

    view.update_in(cx, |v, window, cx| {
        v.focus_tile(tile, cx);
        v.toggle_sized_display(&ToggleSizedDisplay, window, cx);
    });
    assert!(physical_open(&sent(&mut fake, cx)), "back to the physical display");
    let saved = view.read_with(cx, |v, _| v.to_save());
    assert_eq!(saved.displays, [SavedDisplay { tile, sized: false }], "the pick, kept");
    let next = crate::workspace::desktop::Desktop::of(Some(&saved));
    assert_eq!(next.chosen(tile), Some(false), "and read back by the next run");

    // Sized again, the pick is what the device does unasked: nothing left to keep.
    view.update_in(cx, |v, window, cx| v.toggle_sized_display(&ToggleSizedDisplay, window, cx));
    sent(&mut fake, cx);
    assert!(view.read_with(cx, |v, _| v.to_save().displays.is_empty()));
}

/// A stand-in for the session tap: whether macOS would allow it, what it was armed to, and
/// where its chords go.
#[derive(Default)]
struct Keys {
    denied: bool,
    /// macOS keeps its hotkeys: only the tap's list goes.
    chords_only: bool,
    asked: Rc<RefCell<bool>>,
    armed: Rc<RefCell<Vec<bool>>>,
    chords: Rc<RefCell<Option<mpsc::UnboundedSender<Chord>>>>,
}

impl KeyPort for Keys {
    fn install(&mut self, chords: mpsc::UnboundedSender<Chord>) -> bool {
        if self.denied {
            return false;
        }
        self.chords.replace(Some(chords));
        true
    }

    fn arm(&self, on: bool) -> Taking {
        self.armed.borrow_mut().push(on);
        match (on, self.chords_only) {
            (false, _) => Taking::Off,
            (true, false) => Taking::Every,
            (true, true) => Taking::Chords,
        }
    }

    fn ask(&self) {
        self.asked.replace(true);
    }
}

/// Turned on for one window of a remote Mac, the system's shortcuts are taken while that
/// window has the keyboard and go to its worker, and are let be the moment another tile has
/// it; the other window keeps to this Mac, the header shows the state, and turning it off
/// says so. Without Accessibility the person is asked, and the window keeps to this Mac.
#[gpui::test]
fn system_shortcuts_go_to_the_remote_mac_per_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.update(|window, _| window.activate_window());
    let mut fake = connect(&view, cx, 1, "studio");
    let (one, two) = (WindowId(7), WindowId(8));
    let first = arrives(&view, cx, &fake, ItemKind::Window { window: one }, 1);
    let second = arrives(&view, cx, &fake, ItemKind::Window { window: two }, 2);
    opened(&view, cx, &fake, 1, CaptureTarget::Window(one), (1280, 800));
    opened(&view, cx, &fake, 2, CaptureTarget::Window(two), (1280, 800));
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.update(|window, cx| {
        let screen = view.read(cx).screen(first.item).cloned().expect("streaming");
        window.focus(&screen.read(cx).focus_handle(cx), cx);
    });
    cx.run_until_parked();
    assert!(palette_has(&view, cx, SEND_SYSTEM_KEYS), "a Mac's window offers it");

    let denied = Keys { denied: true, ..Keys::default() };
    let asked = Rc::clone(&denied.asked);
    view.update(cx, |v, _| v.set_key_port(Box::new(denied)));
    let toggle = |cx: &mut VisualTestContext| {
        view.update_in(cx, |v, window, cx| v.toggle_system_keys(&ToggleSystemKeys, window, cx));
        cx.run_until_parked();
    };
    let on = |cx: &mut VisualTestContext, tile: TileRef| {
        view.read_with(cx, |v, cx| v.screen(tile.item).is_some_and(|s| s.read(cx).system_keys()))
    };
    toggle(cx);
    assert!(*asked.borrow(), "Accessibility is asked for");
    assert!(!on(cx, first), "and the window keeps to this Mac");
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("Allow Slopty in Accessibility to send system shortcuts"));

    let keys = Keys::default();
    let (armed, chords) = (Rc::clone(&keys.armed), Rc::clone(&keys.chords));
    view.update(cx, |v, _| v.set_key_port(Box::new(keys)));
    toggle(cx);
    assert!(on(cx, first) && !on(cx, second), "per tile");
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("System shortcuts go to studio"));
    assert_eq!(armed.borrow().as_slice(), [true], "taken while it has the keyboard");
    let header = format!("system-keys-{}", first.item.as_uuid());
    assert!(cx.debug_bounds(Box::leak(header.into_boxed_str())).is_some(), "the header says so");
    sent(&mut fake, cx);

    let tab = |down| Chord { code: KeyCode::Tab, down, mods: Mods::SUPER };
    let sender = chords.borrow().clone().expect("tapping");
    for down in [true, false] {
        sender.send(tab(down)).expect("the workspace listens");
    }
    let keys: Vec<(KeyCode, KeyAction, Mods)> = sent(&mut fake, cx)
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Screen(ScreenRequest::Input {
                stream: StreamId(1),
                input: ScreenInput::Key { code, action, mods, .. },
            }) => Some((code, action, mods)),
            _ => None,
        })
        .collect();
    assert_eq!(
        keys,
        [
            (KeyCode::Tab, KeyAction::Press, Mods::SUPER),
            (KeyCode::Tab, KeyAction::Release, Mods::SUPER)
        ]
    );

    view.update_in(cx, |v, _w, cx| v.focus_tile(second, cx));
    cx.run_until_parked();
    assert_eq!(armed.borrow().as_slice(), [true, false], "let be once another tile has it");

    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    toggle(cx);
    assert!(!on(cx, first));
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("System shortcuts stay on this Mac"));
    assert_eq!(armed.borrow().last(), Some(&false));
}

/// A remote window of `fake`'s Mac with the keyboard, system shortcuts on through `keys`: the
/// tile, and what `keys` was armed to.
fn armed_window(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    keys: Keys,
) -> (TileRef, Rc<RefCell<Vec<bool>>>) {
    cx.update(|window, _| window.activate_window());
    let fake = connect(view, cx, 1, "studio");
    let one = WindowId(7);
    let tile = arrives(view, cx, &fake, ItemKind::Window { window: one }, 1);
    opened(view, cx, &fake, 1, CaptureTarget::Window(one), (1280, 800));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.update(|window, cx| {
        let screen = view.read(cx).screen(tile.item).cloned().expect("streaming");
        window.focus(&screen.read(cx).focus_handle(cx), cx);
    });
    let armed = Rc::clone(&keys.armed);
    view.update(cx, |v, _| v.set_key_port(Box::new(keys)));
    view.update_in(cx, |v, window, cx| v.toggle_system_keys(&ToggleSystemKeys, window, cx));
    cx.run_until_parked();
    (tile, armed)
}

/// The window losing the key status lets the shortcuts be as it happens, and taking it back
/// takes them again.
#[gpui::test]
fn the_window_going_inactive_lets_the_shortcuts_be(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (_tile, armed) = armed_window(&view, cx, Keys::default());
    assert_eq!(armed.borrow().as_slice(), [true]);
    cx.deactivate_window();
    assert_eq!(armed.borrow().as_slice(), [true, false], "let be with the window");
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert_eq!(armed.borrow().as_slice(), [true, false, true], "taken again on the way back");
}

/// When macOS keeps its hotkeys and only the tap's own list goes, a notice says so rather
/// than promising every shortcut.
#[gpui::test]
fn a_notice_says_when_only_the_chord_list_goes(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let keys = Keys { chords_only: true, ..Keys::default() };
    let (_tile, armed) = armed_window(&view, cx, keys);
    assert_eq!(armed.borrow().as_slice(), [true]);
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some(ONLY_CHORDS));
}

/// A remote window shown in a window of its own takes the system's shortcuts while that
/// window has the keyboard, as it does in its tile, and lets them be the moment it gives the
/// keyboard up, though the workspace's window draws nothing meanwhile.
#[gpui::test]
fn a_window_of_its_own_takes_the_shortcuts_while_it_is_in_front(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (_tile, armed) = armed_window(&view, cx, Keys::default());
    assert_eq!(armed.borrow().as_slice(), [true]);
    view.update_in(cx, |v, window, cx| v.toggle_own_window(&ToggleOwnWindow, window, cx));
    cx.run_until_parked();
    let popped = cx
        .update(|_, cx| cx.windows())
        .iter()
        .find_map(gpui::AnyWindowHandle::downcast::<PopOutView>)
        .expect("the tile's own window");
    popped.update(cx, |_, window, _| window.activate_window()).expect("open");
    cx.run_until_parked();
    let active = popped.update(cx, |_, window, _| window.is_window_active()).expect("open");
    assert!(active, "its own window is in front");
    assert_eq!(armed.borrow().last(), Some(&true), "taken there: {:?}", armed.borrow());

    // Another app comes to the front: nothing of Slopty's is the key window.
    VisualTestContext::from_window(popped.into(), cx).deactivate_window();
    cx.run_until_parked();
    assert_eq!(armed.borrow().last(), Some(&false), "let be with it: {:?}", armed.borrow());
}

mod app_actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    gpui::actions!(app_under_test, [Quit, Hide, HideOthers]);
}
use app_actions::{Hide, HideOthers, Quit};

/// A remote window with the keyboard keeps its app's chords, so the few of the workspace's it
/// needs take ⌃ on top: ⌃⌘⇧P opens the palette while ⌘⇧P goes to the remote app. Sending system
/// shortcuts also sends ⌘Q and ⌘H, which otherwise quit and hide this app.
#[gpui::test]
fn a_remote_window_keeps_its_chords_and_reaches_ours_with_control(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let quits = Rc::new(std::cell::Cell::new(0_u32));
    cx.update(|_, cx| {
        cx.bind_keys(crate::keymap::app_chords(Quit, Hide, HideOthers));
        let counted = Rc::clone(&quits);
        cx.on_action(move |_: &Quit, _| counted.set(counted.get().saturating_add(1)));
    });
    cx.update(|window, _| window.activate_window());
    let mut fake = connect(&view, cx, 1, "studio");
    let one = WindowId(7);
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window: one }, 1);
    opened(&view, cx, &fake, 1, CaptureTarget::Window(one), (1280, 800));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.update(|window, cx| {
        let screen = view.read(cx).screen(tile.item).cloned().expect("streaming");
        window.focus(&screen.read(cx).focus_handle(cx), cx);
    });
    view.update(cx, |v, _| v.set_key_port(Box::new(Keys::default())));
    view.update_in(cx, |v, window, cx| v.toggle_system_keys(&ToggleSystemKeys, window, cx));
    cx.run_until_parked();
    sent(&mut fake, cx);
    let pressed = |fake: &mut Fake, cx: &mut VisualTestContext| -> Vec<(KeyCode, Mods)> {
        sent(fake, cx)
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Screen(ScreenRequest::Input {
                    input: ScreenInput::Key { code, action: KeyAction::Press, mods },
                    ..
                }) => Some((code, mods)),
                _ => None,
            })
            .collect()
    };
    cx.simulate_keystrokes("cmd-shift-p");
    assert!(!view.read_with(cx, |v, _| v.palette_open()), "⌘⇧P is the remote app's");
    assert_eq!(pressed(&mut fake, cx), [(KeyCode::P, Mods::SUPER | Mods::SHIFT)]);
    cx.simulate_keystrokes("cmd-q");
    assert_eq!(quits.get(), 0, "⌘Q quits the remote app, not this one");
    assert_eq!(pressed(&mut fake, cx), [(KeyCode::Q, Mods::SUPER)]);
    cx.simulate_keystrokes("ctrl-cmd-shift-p");
    assert!(view.read_with(cx, |v, _| v.palette_open()), "⌃⌘⇧P opens ours");
    assert!(pressed(&mut fake, cx).is_empty(), "and nothing goes");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.palette_open()));

    // System shortcuts back on this Mac: ⌘Q is the app's again.
    view.update_in(cx, |v, window, cx| v.toggle_system_keys(&ToggleSystemKeys, window, cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-q");
    assert_eq!(quits.get(), 1, "⌘Q is this app's");
}

/// A remote password field with the keyboard holds secure keyboard entry here, as a shell's
/// password prompt does, and lets go once the worker says another field has it.
#[gpui::test]
fn a_remote_password_field_holds_secure_entry(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.update(|window, _| window.activate_window());
    let fake = connect(&view, cx, 1, "studio");
    let one = WindowId(7);
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window: one }, 1);
    opened(&view, cx, &fake, 1, CaptureTarget::Window(one), (1280, 800));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    let on = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.secure_input_on());
    let field = |secure| ScreenEvent::Field {
        stream: StreamId(1),
        field: Some(slopty_proto::screen::TextField { caret: None, secure }),
    };
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| v.screen_event(key, field(true), cx));
    cx.run_until_parked();
    assert!(on(cx), "a password field has the keyboard");
    view.update_in(cx, |v, _w, cx| v.screen_event(key, field(false), cx));
    cx.run_until_parked();
    assert!(!on(cx), "a plain field");
}

/// The curtain over a remote Mac is this device's to hold from a desktop tile: offered only on
/// a worker that draws one; asked for from the palette, and shown in the tile's header while
/// held; asked again when the link comes back; a refusal lets the hold go and says why, as a
/// curtain up without the Mac's own input held says what that needs; and lifted, let go.
#[gpui::test]
fn the_curtain_is_held_from_a_desktop_tile_and_asked_again_on_each_link(cx: &mut TestAppContext) {
    use slopty_proto::screen::CurtainState;

    use crate::workspace::actions::ToggleCurtain;
    use crate::workspace::desktop::{DRAW_CURTAIN, LIFT_CURTAIN};

    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let physical = DisplayId(1);
    let tile = arrives(&view, cx, &fake, ItemKind::Display { display: physical }, 1);
    opened(&view, cx, &fake, 1, CaptureTarget::Display(physical), (2560, 1440));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    assert!(!palette_has(&view, cx, DRAW_CURTAIN), "this worker draws none");

    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(key, WorkerCaps { curtain: true, ..healthy() }, cx);
    });
    cx.run_until_parked();
    assert!(palette_has(&view, cx, DRAW_CURTAIN));
    sent(&mut fake, cx);
    let asks = |fake: &mut Fake, cx: &VisualTestContext| -> Vec<bool> {
        sent(fake, cx)
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Screen(ScreenRequest::Curtain { on }) => Some(on),
                _ => None,
            })
            .collect()
    };
    let toggle = |cx: &mut VisualTestContext| {
        view.update_in(cx, |v, window, cx| v.toggle_curtain(&ToggleCurtain, window, cx));
        cx.run_until_parked();
    };
    let hear = |cx: &mut VisualTestContext, state: CurtainState| {
        view.update_in(cx, |v, _w, cx| v.screen_event(key, ScreenEvent::Curtain(state), cx));
        cx.run_until_parked();
    };
    let header = format!("curtain-{}", tile.item.as_uuid());
    let header: &'static str = Box::leak(header.into_boxed_str());

    toggle(cx);
    assert_eq!(asks(&mut fake, cx), [true]);
    hear(cx, CurtainState::Up { holders: 1, input_held: false });
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(
        said.as_deref(),
        Some(
            "studio's screens are covered, but its own keyboard and pointer still work: allow \
             Slopty in Accessibility there"
        )
    );
    assert!(palette_has(&view, cx, LIFT_CURTAIN));
    assert!(cx.debug_bounds(header).is_some(), "the header says it is held");

    let (tx, rx) = mpsc::channel(256);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    let me = fake.me;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
        let link = WorkerLink { me, out: tx, open_screen: factory, remote: None };
        v.connect_worker(key, link, hello("studio", Vec::new()), cx);
    });
    fake.rx = rx;
    assert_eq!(asks(&mut fake, cx), [true], "held again on the new link");

    hear(cx, CurtainState::Refused { why: "the worker's main thread is gone".to_owned() });
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("No curtain over studio: the worker's main thread is gone"));
    assert!(!view.read_with(cx, |v, _| v.holds_curtain(key)), "the hold went with the refusal");

    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(key, WorkerCaps { curtain: true, ..healthy() }, cx);
    });
    toggle(cx);
    hear(cx, CurtainState::Up { holders: 1, input_held: true });
    toggle(cx);
    assert_eq!(asks(&mut fake, cx), [true, false], "drawn, then lifted");
    hear(cx, CurtainState::Down);
    assert!(palette_has(&view, cx, DRAW_CURTAIN));
}

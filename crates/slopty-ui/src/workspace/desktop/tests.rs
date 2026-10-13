//! The workspace's side of a remote window's controls, headless: one worker on a fake link,
//! one window or desktop streaming in a window of its own, and the rows of its controls asked
//! for as the view asks for them. The system's shortcuts go through a stand-in for the session
//! tap: a test never makes a tap, so it never asks for Accessibility.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AppContext as _, ClipboardItem, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, VisualTestContext, div, px, size,
};
use slopty_core::{ClientId, DisplayId, ItemId, StreamId, WindowId, WorkerId};
use slopty_proto::handshake::HelloAck;
use slopty_proto::input::{KeyAction, KeyCode, Mods};
use slopty_proto::items::{Item, ItemSync};
use slopty_proto::screen::{CaptureTarget, ScreenEvent, ScreenInput, VideoCodec};
use slopty_proto::server::{Os, WorkerCaps};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use super::*;
use crate::workspace::{WorkerLink, WorkerStatus, WorkspaceEvent};

/// The one worker's key.
const KEY: WorkerKey = WorkerKey::new(1);

/// This device's display key, set so a test never draws one in the data directory.
const DEVICE: DisplayKey = DisplayKey([7; 16]);

/// What the worker was sent, and the notices the workspace said.
struct Fake {
    rx: mpsc::Receiver<ClientMsg>,
    notices: Rc<RefCell<Vec<String>>>,
}

impl Fake {
    /// Everything sent so far, paced typing given its frames.
    fn sent(&mut self, cx: &VisualTestContext) -> Vec<ClientMsg> {
        let mut all = Vec::new();
        let mut quiet = 0_u8;
        while quiet < 3 {
            cx.run_until_parked();
            let more: Vec<ClientMsg> = std::iter::from_fn(|| self.rx.try_recv().ok()).collect();
            quiet = if more.is_empty() { quiet.saturating_add(1) } else { 0 };
            all.extend(more);
            cx.executor().advance_clock(Duration::from_millis(16));
        }
        all
    }

    /// The last notice said.
    fn said(&self) -> Option<String> {
        self.notices.borrow().last().cloned()
    }
}

/// A window that shows the workspace's view of one item, as the shell's surface does.
struct Host {
    workspace: Entity<WorkspaceView>,
    item: ItemId,
}

impl Render for Host {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.workspace.read(cx).screen(self.item).cloned())
    }
}

/// A worker that is a Mac and can do what `caps` adds.
fn mac(caps: impl FnOnce(&mut WorkerCaps)) -> WorkerCaps {
    let mut out = WorkerCaps { can_capture: true, can_inject: true, ..WorkerCaps::bare(Os::MacOs) };
    caps(&mut out);
    out
}

/// The link to the worker comes up: what it is sent goes to the returned channel.
fn link(
    workspace: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    caps: WorkerCaps,
) -> mpsc::Receiver<ClientMsg> {
    let (out, rx) = mpsc::channel(256);
    let link = WorkerLink {
        me: ClientId::new(),
        out,
        open_screen: Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream)),
        remote: None,
    };
    let ack = HelloAck {
        worker: WorkerId::new(),
        name: "studio".to_owned(),
        home: String::new(),
        settings: String::new(),
        caps,
        load: 0.0,
        sessions: Vec::new(),
    };
    workspace.update(cx, |v, cx| v.connect_worker(KEY, link, ack, cx));
    cx.run_until_parked();
    rx
}

/// A workspace whose one worker, linked with `caps`, holds `kind`, streaming as `stream` at
/// `pixels` in a 400 × 300 window: the workspace, the window's host, the item and the fake.
fn streaming(
    cx: &mut TestAppContext,
    caps: WorkerCaps,
    kind: ItemKind,
    pixels: (u32, u32),
) -> (Entity<WorkspaceView>, Entity<Host>, ItemId, Fake, &mut VisualTestContext) {
    let workspace = cx.new(|_| {
        let mut v = WorkspaceView::new(Theme::default());
        v.desktop.key = Some(DEVICE);
        v.desktop.keys = None;
        v
    });
    let notices = Rc::new(RefCell::new(Vec::new()));
    let heard = Rc::clone(&notices);
    cx.update(|cx| {
        cx.subscribe(&workspace, move |_, event, _| {
            if let WorkspaceEvent::Notice { text, .. } = event {
                heard.borrow_mut().push(text.clone());
            }
        })
        .detach();
    });
    let item = ItemId::new();
    let shown = workspace.clone();
    let (host, cx) = cx.add_window_view(|_window, cx| {
        cx.observe(&shown, |_, _, cx| cx.notify()).detach();
        Host { workspace: shown, item }
    });
    cx.simulate_resize(size(px(400.0), px(300.0)));
    workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
    let rx = link(&workspace, cx, caps);
    workspace.update(cx, |v, cx| {
        let items = vec![Item { id: item, kind, name: None, facts: BTreeMap::new() }];
        v.apply_sync(KEY, ItemSync::Snapshot { version: 1, items }, cx);
    });
    let target = match kind {
        ItemKind::Window { window } => CaptureTarget::Window(window),
        ItemKind::Display { display } => CaptureTarget::Display(display),
        ItemKind::Terminal { .. } => panic!("a terminal does not stream"),
    };
    let mut fake = Fake { rx, notices };
    assert!(
        fake.sent(cx).iter().any(|m| matches!(
            m,
            ClientMsg::Screen(ScreenRequest::Open { target: t, .. }) if *t == target
        )),
        "the item is asked for"
    );
    opened(&workspace, cx, StreamId(1), target, pixels);
    (workspace, host, item, fake, cx)
}

/// The worker opened `stream` of `target` at `pixels`; the window draws its view.
fn opened(
    workspace: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    stream: StreamId,
    target: CaptureTarget,
    (width, height): (u32, u32),
) {
    workspace.update(cx, |v, cx| {
        let event = ScreenEvent::Opened {
            stream,
            target,
            codec: VideoCodec::Hevc,
            width,
            height,
            scale: 2.0,
            stripes: Vec::new(),
        };
        v.screen_event(KEY, event, cx);
        cx.notify();
    });
    cx.run_until_parked();
    cx.executor().advance_clock(crate::screen::fit::SETTLE);
    cx.run_until_parked();
}

/// The view of `item`, which must be streaming.
fn view_of(
    workspace: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
    item: ItemId,
) -> Entity<ScreenView> {
    workspace.read_with(cx, |v, _| v.screen(item).cloned()).expect("streaming")
}

/// What the view of `item` is offered.
fn offers(workspace: &Entity<WorkspaceView>, cx: &VisualTestContext, item: ItemId) -> Offers {
    view_of(workspace, cx, item).read_with(cx, |v, _| v.offers())
}

/// A row of the view of `item`'s controls asked for `control`.
fn ask(
    workspace: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    item: ItemId,
    control: Control,
) {
    workspace.update(cx, |v, cx| v.screen_control(item, control, cx));
    cx.run_until_parked();
}

/// The curtain is offered on a desktop of a worker that draws one, held from this device and
/// asked again on each link; the worker refusing it lets the hold go and says why, and with
/// the worker out of reach it stays as it is.
#[gpui::test]
fn the_curtain_is_held_from_this_device_and_asked_again_on_each_link(cx: &mut TestAppContext) {
    let display = ItemKind::Display { display: DisplayId(1) };
    let caps = mac(|c| c.curtain = true);
    let (workspace, _host, item, mut fake, cx) = streaming(cx, caps.clone(), display, (800, 600));
    assert_eq!(offers(&workspace, cx, item).curtain, Some(false), "offered, not held");
    let curtains = |msgs: &[ClientMsg]| {
        msgs.iter()
            .filter_map(|m| match m {
                ClientMsg::Screen(ScreenRequest::Curtain { on }) => Some(*on),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    fake.sent(cx);

    ask(&workspace, cx, item, Control::Curtain);
    assert_eq!(curtains(&fake.sent(cx)), [true]);
    assert_eq!(offers(&workspace, cx, item).curtain, Some(true), "ticked while held");

    workspace.update(cx, |v, cx| {
        v.disconnect_worker(KEY, WorkerStatus::Relinking, cx);
    });
    let mut again = link(&workspace, cx, caps);
    let resent: Vec<ClientMsg> = std::iter::from_fn(|| again.try_recv().ok()).collect();
    assert_eq!(curtains(&resent), [true], "the hold is asked again on the new link");
    fake.rx = again;

    let refused = CurtainState::Refused { why: "screen sharing is off".to_owned() };
    workspace.update(cx, |v, cx| v.screen_event(KEY, ScreenEvent::Curtain(refused), cx));
    cx.run_until_parked();
    assert_eq!(fake.said().as_deref(), Some("No curtain over studio: screen sharing is off"));
    assert_eq!(offers(&workspace, cx, item).curtain, Some(false), "the hold let go");

    workspace.update(cx, |v, cx| {
        v.disconnect_worker(KEY, WorkerStatus::Relinking, cx);
    });
    ask(&workspace, cx, item, Control::Curtain);
    let said = fake.said();
    assert_eq!(said.as_deref(), Some("studio is out of reach, so the curtain stays as it is"));
    assert_eq!(offers(&workspace, cx, item).curtain, Some(false));
}

/// A desktop of a worker that makes displays streams from one made for this device at its
/// view's size and the screen's scale in place of the physical one, follows the view once it
/// holds a new size, and goes back to the physical display when asked again. A worker that
/// makes none is never offered it.
#[gpui::test]
fn a_display_made_for_this_device_follows_its_view(cx: &mut TestAppContext) {
    let physical = DisplayId(1);
    let display = ItemKind::Display { display: physical };
    let (workspace, _host, item, mut fake, cx) = streaming(cx, mac(|_| {}), display, (2560, 1440));
    assert_eq!(offers(&workspace, cx, item).sized, None, "this worker makes no displays");
    workspace.update(cx, |v, cx| v.set_worker_caps(KEY, mac(|c| c.virtual_displays = true), cx));
    cx.run_until_parked();
    assert_eq!(offers(&workspace, cx, item).sized, Some(false));
    fake.sent(cx);

    ask(&workspace, cx, item, Control::SizedDisplay);
    let asked = fake.sent(cx);
    let scale = cx.update(|window, _| window.scale_factor());
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
    let body = |w: f32, h: f32| ((w * scale).round() as u32, (h * scale).round() as u32);
    let shape = asked
        .iter()
        .find_map(|m| match m {
            ClientMsg::Screen(ScreenRequest::OpenDisplay { key, shape, .. }) => {
                assert_eq!(*key, DEVICE, "this device's key");
                Some(*shape)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("a display asked for: {asked:?}"));
    assert_eq!((shape.width, shape.height), body(400.0, 300.0), "sized to the view's body");
    assert!((shape.scale - scale).abs() < f32::EPSILON, "at the screen's scale");
    assert!(
        asked.iter().any(|m| matches!(m, ClientMsg::Screen(ScreenRequest::Close(StreamId(1))))),
        "the physical display's stream goes: {asked:?}"
    );

    let made = DisplayId(9);
    workspace.update(cx, |v, cx| {
        let told = ScreenEvent::Display {
            stream: StreamId(2),
            key: DEVICE,
            display: VirtualDisplay::Made(made),
        };
        v.screen_event(KEY, told, cx);
    });
    opened(&workspace, cx, StreamId(2), CaptureTarget::Display(made), body(400.0, 300.0));
    let view = view_of(&workspace, cx, item);
    assert_eq!(view.read_with(cx, |v, _| v.stream()), StreamId(2), "the made display shows");
    assert_eq!(offers(&workspace, cx, item).sized, Some(true), "ticked");
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
    assert!(resized(&fake.sent(cx)).is_empty(), "its own size asks nothing");

    cx.simulate_resize(size(px(600.0), px(300.0)));
    cx.run_until_parked();
    assert!(resized(&fake.sent(cx)).is_empty(), "not while it may still move");
    cx.executor().advance_clock(crate::screen::fit::SETTLE);
    cx.run_until_parked();
    let wider = body(600.0, 300.0);
    assert_eq!(resized(&fake.sent(cx)), [(StreamId(2), wider, Some(scale))], "once it held");

    ask(&workspace, cx, item, Control::SizedDisplay);
    let back = fake.sent(cx);
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

/// A window is never offered a display of its own, whatever its worker makes.
#[gpui::test]
fn a_window_is_never_offered_a_display(cx: &mut TestAppContext) {
    let window = ItemKind::Window { window: WindowId(3) };
    let caps = mac(|c| c.virtual_displays = true);
    let (workspace, _host, item, _fake, cx) = streaming(cx, caps, window, (800, 600));
    assert_eq!(offers(&workspace, cx, item).sized, None);
}

/// What the stand-in tap was asked, and its way to hand chords in.
#[derive(Default)]
struct Tapped {
    installs: usize,
    arms: Vec<bool>,
    asked: usize,
    chords: Option<mpsc::UnboundedSender<Chord>>,
}

/// A stand-in for the session tap: taps when `allowed`, else needs Accessibility.
struct Keys {
    allowed: bool,
    taking: Taking,
    seen: Rc<RefCell<Tapped>>,
}

impl KeyPort for Keys {
    fn install(&mut self, chords: mpsc::UnboundedSender<Chord>) -> bool {
        let mut seen = self.seen.borrow_mut();
        seen.installs = seen.installs.saturating_add(1);
        if self.allowed {
            seen.chords = Some(chords);
        }
        self.allowed
    }

    fn arm(&self, on: bool) -> Taking {
        self.seen.borrow_mut().arms.push(on);
        if on { self.taking } else { Taking::Off }
    }

    fn ask(&self) {
        let mut seen = self.seen.borrow_mut();
        seen.asked = seen.asked.saturating_add(1);
    }
}

/// The view of `item` takes the keyboard in its window.
fn focus(workspace: &Entity<WorkspaceView>, cx: &mut VisualTestContext, item: ItemId) {
    let view = view_of(workspace, cx, item);
    cx.update(|window, cx| {
        window.activate_window();
        window.focus(&view.read(cx).focus_handle(cx), cx);
    });
    cx.run_until_parked();
}

/// On a remote Mac, the system's shortcuts go to the window whose view has the keyboard once
/// its row turns them on: the tap is made the first time, armed while that view has the
/// keyboard and let be the moment it loses it. A chord taken goes to the worker as its key.
/// Without Accessibility the person is asked for it and the shortcuts stay here.
#[gpui::test]
fn system_shortcuts_go_to_the_remote_mac_while_its_view_has_the_keyboard(cx: &mut TestAppContext) {
    let window = ItemKind::Window { window: WindowId(7) };
    let (workspace, _host, item, mut fake, cx) = streaming(cx, mac(|_| {}), window, (800, 600));
    assert_eq!(offers(&workspace, cx, item).system_keys, None, "nothing takes them here");

    let seen = Rc::new(RefCell::new(Tapped::default()));
    let denied = Keys { allowed: false, taking: Taking::Every, seen: Rc::clone(&seen) };
    workspace.update(cx, |v, cx| {
        v.desktop.keys = Some(Box::new(denied));
        v.refresh_offers(cx);
    });
    assert_eq!(offers(&workspace, cx, item).system_keys, Some(false), "offered on a Mac");
    ask(&workspace, cx, item, Control::SystemKeys);
    assert_eq!(seen.borrow().asked, 1, "Accessibility asked for");
    assert_eq!(fake.said().as_deref(), Some(ASK_ACCESSIBILITY));
    assert_eq!(offers(&workspace, cx, item).system_keys, Some(false), "they stay here");

    let seen = Rc::new(RefCell::new(Tapped::default()));
    let keys = Keys { allowed: true, taking: Taking::Every, seen: Rc::clone(&seen) };
    workspace.update(cx, |v, _| v.desktop.keys = Some(Box::new(keys)));
    focus(&workspace, cx, item);
    ask(&workspace, cx, item, Control::SystemKeys);
    assert_eq!(offers(&workspace, cx, item).system_keys, Some(true), "ticked");
    assert_eq!(seen.borrow().installs, 1, "the tap made once");
    assert_eq!(seen.borrow().arms, [true], "armed: the view has the keyboard");
    assert_eq!(fake.said().as_deref(), Some("System shortcuts go to studio"));
    fake.sent(cx);

    let tab = Chord { code: KeyCode::Tab, down: true, mods: Mods::SUPER };
    let chords = seen.borrow().chords.clone().expect("the tap's way in");
    chords.send(tab).expect("the workspace listens");
    let keys: Vec<(KeyCode, Mods)> = fake
        .sent(cx)
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Screen(ScreenRequest::Input {
                input: ScreenInput::Key { code, action: KeyAction::Press, mods, .. },
                ..
            }) => Some((code, mods)),
            _ => None,
        })
        .collect();
    assert_eq!(keys, [(KeyCode::Tab, Mods::SUPER)], "⌘Tab went to the worker");

    cx.update(gpui::Window::blur);
    cx.run_until_parked();
    assert_eq!(seen.borrow().arms, [true, false], "let be when the view lost the keyboard");
}

/// The clipboard's text is typed into the window, a line break as Return; past
/// [`TYPE_MAX`] only the first of it, never a character cut in half, and a notice says so; an
/// empty clipboard types nothing and says why.
#[gpui::test]
fn the_clipboard_is_typed_into_the_window(cx: &mut TestAppContext) {
    let window = ItemKind::Window { window: WindowId(7) };
    let (workspace, _host, item, mut fake, cx) = streaming(cx, mac(|_| {}), window, (800, 600));
    assert!(offers(&workspace, cx, item).type_clipboard);
    fake.sent(cx);
    let typed = |msgs: Vec<ClientMsg>| {
        msgs.into_iter()
            .filter_map(|m| match m {
                ClientMsg::Screen(ScreenRequest::Input { input, .. }) => match input {
                    ScreenInput::Text { text } => Some(text),
                    ScreenInput::Key { code, action: KeyAction::Press, .. } => {
                        Some(format!("{code:?}"))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    cx.write_to_clipboard(ClipboardItem::new_string("ok\r\nA".to_owned()));
    ask(&workspace, cx, item, Control::TypeClipboard);
    assert_eq!(typed(fake.sent(cx)), ["ok", "Enter", "A"]);

    cx.write_to_clipboard(ClipboardItem::new_string("é".repeat(600)));
    ask(&workspace, cx, item, Control::TypeClipboard);
    assert_eq!(typed(fake.sent(cx)).concat(), "é".repeat(TYPE_MAX / 2), "none cut in half");
    assert_eq!(fake.said().as_deref(), Some("Typed the first 1 KB of the clipboard"));

    cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
    ask(&workspace, cx, item, Control::TypeClipboard);
    assert_eq!(typed(fake.sent(cx)), Vec::<String>::new());
    assert_eq!(fake.said().as_deref(), Some("The clipboard holds no text"));
}

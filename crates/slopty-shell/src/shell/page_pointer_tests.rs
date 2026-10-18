// slopty: Slopty's own tests (not in zeron).
//! slopty: a chat at device scale, the whole shell drawn headless: live pages take the pointer
//! (judged by the hit map the window presents, which the platform's hit testing reads), and a
//! chat at its end stays there when its floor moves.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use chrono::TimeDelta;
use gpui::composition::{NativeHost, NativeHostOptions};
use gpui::{TestAppContext, VisualTestContext};
use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, PageCard, SessionMessageEntry};
use slopty_view_proto::{Session, SessionStatus};

use super::*;
use crate::live::{self, HostedPage, PagePresence};

/// The iPad's window, in points.
const PAD: (f32, f32) = (1032.0, 1376.0);

/// Serve every page live, each placed with a native host of the window's own, as tall as
/// `height` says for its page.
fn serve_live(cx: &mut App, height: impl Fn(&str) -> f32 + 'static) {
    let natives: Rc<RefCell<HashMap<String, NativeHost>>> = Rc::default();
    live::serve_pages(
        cx,
        move |page, window, cx| {
            let host = natives
                .borrow_mut()
                .entry(page.to_owned())
                .or_insert_with(|| {
                    window.create_native_host(NativeHostOptions::default(), cx).unwrap()
                })
                .clone();
            Some(HostedPage {
                presence: PagePresence::Live,
                native: Some(host),
                snapshot: None,
                height: Some(height(page)),
                failed: None,
                waiting: None,
                doc: None,
            })
        },
        |_, _, _| {},
        |_, _, _| None,
    );
}

/// A shell window `size` big, with one finished chat, not open yet, reduced motion on.
fn shell(cx: &mut TestAppContext, size: (f32, f32)) -> (Entity<Shell>, &mut VisualTestContext) {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        settings::init(UiSettings::default(), dir.path(), cx);
        crate::composer::init(cx, ComposerSendBehavior::default());
        apply_keymap(cx, &KeymapConfig::default(), ComposerSendBehavior::default());
        cx.set_reduce_motion(true);
    });
    let path = dir.path().to_path_buf();
    let (shell, cx) = cx.add_window_view(move |_, cx| {
        let state = cx.new(|_| AppState::new());
        let boot = EngineBootConfig {
            data_dir: path.clone(),
            default_harness: slopty_view_proto::HarnessId::MOCK,
        };
        let mut shell = Shell::new(state, boot, cx);
        shell.debug_gate = Some(GatePhase::Ready);
        shell.splash = SplashPhase::Gone;
        let now = Utc::now();
        let chat: slopty_view_proto::Chat = serde_json::from_value(serde_json::json!({
            "id": "a", "title": "Chat a", "deviceId": "local", "archived": false,
            "createdAt": now - TimeDelta::minutes(1), "cwd": "/work/app",
            "lastMessageAt": now - TimeDelta::seconds(30), "lastSeenAt": now,
        }))
        .unwrap();
        let session = Session {
            last_completed_turn: None,
            chat_id: "a".into(),
            device_id: "local".into(),
            status: SessionStatus::Done,
            started_at: Some(now - TimeDelta::minutes(1)),
            updated_at: now,
            running_subagents: 0,
            status_since: Some(now - TimeDelta::seconds(20)),
            waiting_on: None,
        };
        shell.state.update(cx, |state, _| {
            state.connection = slopty_view_proto::view::ConnectionStatus::Ready;
            state.workspace_scope = Some(WorkspaceScope::Local);
            state.local_device_id = Some("local".into());
            state.chats = vec![chat];
            state.apply_sessions(vec![session]);
        });
        shell
    });
    cx.simulate_resize(gpui::size(px(size.0), px(size.1)));
    cx.run_until_parked();
    (shell, cx)
}

/// An HTML page's card.
fn html_card(page: &str, title: &str) -> PageCard {
    PageCard {
        id: format!("{page}:page"),
        page: page.into(),
        title: title.into(),
        kind: PageCard::HTML.into(),
        source: "/work/out/page.html".into(),
        ..PageCard::default()
    }
}

/// A finished turn whose answer shows `pages`, each followed by a line.
fn turn_with_pages(chat: &str, pages: &[PageCard]) -> Vec<SessionMessageEntry> {
    let now = Utc::now().timestamp_millis();
    let text = |id: String, text: &str| MessagePart::Text { id, text: text.into() };
    let mut parts = vec![text(format!("{chat}:answer:t"), "Here are the pages.")];
    for card in pages {
        parts.push(MessagePart::Page { id: card.id.clone(), card: Box::new(card.clone()) });
        parts.push(text(format!("{}:after", card.id), "And after it, a line."));
    }
    let entry = |id: String, role, parts, status| SessionMessageEntry {
        id,
        role,
        parts,
        created_at: now,
        device_id: "local".into(),
        status,
        continuation_of: None,
        duration_ms: None,
    };
    let ask = vec![text(format!("{chat}:ask:t"), "Show me the pages")];
    vec![
        entry(format!("{chat}:ask"), MessageRole::User, ask, None),
        entry(
            format!("{chat}:answer"),
            MessageRole::Assistant,
            parts,
            Some(MessageStatus::Complete),
        ),
    ]
}

/// A chat whose answer shows a tall page and a short one, open in the shell at the iPad's size:
/// each live page takes the pointer at its middle. The chat's two drop overlays, clear and over
/// the whole chat at rest, lay their hitboxes over every page, so on the iPad no finger reached
/// one, nor on the Mac a click (e2e `pages`, run 42); they are drawn only during a drag now.
#[gpui::test]
fn live_pages_in_a_chat_take_the_pointer_at_the_ipads_size(cx: &mut TestAppContext) {
    cx.update(|cx| serve_live(cx, |page| if page.ends_with("tall") { 900.0 } else { 160.0 }));
    let (shell, cx) = shell(cx, PAD);
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    let pages =
        [html_card("slopty-page:a/tall", "Tall"), html_card("slopty-page:a/short", "Short")];
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_transcript(turn_with_pages("a", &pages));
            cx.notify();
        });
    });
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    let (takes, map) = cx.update(|window, cx| {
        window.draw_and_present(cx);
        let map = window.native_hit_map();
        let takes: Vec<bool> = map.as_ref().map_or_else(Vec::new, |map| {
            let at = |entry: &gpui::composition::NativeHitEntry| entry.visible.center();
            map.entries.iter().map(|entry| map.native_at(at(entry)).is_some()).collect()
        });
        (takes, format!("{map:#?}"))
    });
    assert_eq!(takes.len(), 2, "both live pages are placed:\n{map}");
    assert!(takes.iter().all(|&t| t), "each page takes the pointer over it:\n{map}");
}

/// The iPhone's window, in points.
const PHONE: (f32, f32) = (402.0, 874.0);

/// What the host was asked of each page, in order.
type Acts = Rc<RefCell<Vec<(String, live::PageAct)>>>;

/// Serve every page warm until it is engaged, as the app's host does: an engaged page goes live
/// (and the one live before it rests: a phone's cap is one), a rested one goes warm, and every
/// card hears what moved. Tall pages ask for more than any card shows.
fn serve_engaged(cx: &mut App) -> (Rc<RefCell<HashMap<String, PagePresence>>>, Acts) {
    use crate::live::PageAct;
    type Watchers = Rc<RefCell<Vec<Box<dyn FnMut(&mut App)>>>>;
    let presence: Rc<RefCell<HashMap<String, PagePresence>>> = Rc::default();
    let natives: Rc<RefCell<HashMap<String, NativeHost>>> = Rc::default();
    let acts: Acts = Rc::default();
    let watchers: Watchers = Rc::default();
    let (served, acted, heard, told, watched) =
        (presence.clone(), presence.clone(), acts.clone(), watchers.clone(), watchers);
    live::serve_pages(
        cx,
        move |page, window, cx| {
            let now = *served.borrow().get(page).unwrap_or(&PagePresence::Warm);
            let native = (now == PagePresence::Live).then(|| {
                natives
                    .borrow_mut()
                    .entry(page.to_owned())
                    .or_insert_with(|| {
                        window.create_native_host(NativeHostOptions::default(), cx).unwrap()
                    })
                    .clone()
            });
            Some(HostedPage {
                presence: now,
                native,
                snapshot: None,
                height: Some(if page.ends_with("tall") { 5000.0 } else { 160.0 }),
                failed: None,
                waiting: None,
                doc: None,
            })
        },
        move |page, act, cx| {
            heard.borrow_mut().push((page.to_owned(), act));
            let mut map = acted.borrow_mut();
            match act {
                PageAct::Engage => {
                    map.values_mut().filter(|v| **v == PagePresence::Live).for_each(|v| {
                        *v = PagePresence::Warm;
                    });
                    map.insert(page.to_owned(), PagePresence::Live);
                }
                PageAct::Rest => {
                    map.insert(page.to_owned(), PagePresence::Warm);
                }
                _ => return,
            }
            drop(map);
            let told = told.clone();
            cx.defer(move |cx| {
                let mut all = std::mem::take(&mut *told.borrow_mut());
                all.iter_mut().for_each(|f| f(cx));
                told.borrow_mut().extend(all);
            });
        },
        move |_, changed, _| {
            watched.borrow_mut().push(changed);
            None
        },
    );
    (presence, acts)
}

/// slopty: on an iPhone a page as tall as a card goes (0.8 of the window) is taller than the
/// band a live page may show in, below the titlebar's fade and above the composer's. A press
/// on it made it live, the band rested it at once, and its picture took the hover back and
/// engaged it again, for ever (e2e `pages`: the card never stayed live, and a finger found its
/// web view half the time). It goes live and stays so, its page shown and taking the pointer
/// within the band alone.
#[gpui::test]
fn a_page_taller_than_the_phones_band_stays_live_where_it_shows(cx: &mut TestAppContext) {
    super::compact::force_touch(true);
    let (presence, acts) = cx.update(serve_engaged);
    let (shell, cx) = shell(cx, PHONE);
    cx.simulate_scale_factor_change(3.0);
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    let pages =
        [html_card("slopty-page:a/short", "Short"), html_card("slopty-page:a/tall", "Tall")];
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_transcript(turn_with_pages("a", &pages));
            cx.notify();
        });
    });
    let frames = |cx: &mut VisualTestContext| {
        for _ in 0..4 {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw_and_present(cx));
        }
    };
    cx.executor().advance_clock(Duration::from_millis(500));
    frames(cx);
    let said = |acts: &Acts, page: &str, act: &live::PageAct| {
        acts.borrow().iter().filter(|(p, a)| p == page && a == act).count()
    };
    let tall = "slopty-page:a/tall";

    // The chat opens at its end, the tall page over its last line; a press in what shows of it.
    let at = gpui::point(px(200.0), px(400.0));
    cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    frames(cx);
    // Past the 2 s a page stays live once the pointer has left it.
    cx.executor().advance_clock(Duration::from_secs(3));
    frames(cx);
    let log = || format!("{:?}", acts.borrow());
    assert_eq!(said(&acts, tall, &live::PageAct::Engage), 1, "one press, one wake: {}", log());
    assert_eq!(said(&acts, tall, &live::PageAct::Rest), 0, "it stays live: {}", log());
    assert_eq!(presence.borrow().get(tall), Some(&PagePresence::Live));
    // The band's top: under the titlebar and its fade, over the chat's list.
    let list = shell.update(cx, |shell, cx| shell.transcript.read(cx).list_state().viewport_bounds());
    let top = f32::from(list.top()) + Theme::TITLEBAR_HEIGHT + Theme::TRANSCRIPT_FADE_BAND;
    let (map, said_map) = cx.update(|window, _| {
        let map = window.native_hit_map().expect("a native presented");
        let said = format!("{map:#?}");
        (map, said)
    });
    assert_eq!(map.entries.len(), 1, "the tall page alone is placed:\n{said_map}");
    let shows = map.entries[0].visible;
    assert!(
        f32::from(shows.top()) >= top - 0.5 && f32::from(shows.bottom()) < f32::from(list.bottom()),
        "it shows within the band alone, from {top} and over the composer: {shows:?}"
    );
    assert!(map.native_at(at).is_some(), "the page takes the pointer where it shows");
    let in_fade = gpui::point(px(200.0), px(top - 10.0));
    assert!(map.native_at(in_fade).is_none(), "under the titlebar's fade the chat has it");

}

/// slopty: a chat at its end stays at its end when its floor moves: on a phone the keyboard
/// rising makes the chat's view shorter, and its last line went under the composer (audit #9,
/// `ios-phone-compact-paste`). A chat the person scrolled up stays where they left it.
#[gpui::test]
fn a_chat_at_its_end_stays_there_when_the_phones_floor_rises(cx: &mut TestAppContext) {
    super::compact::force_touch(true);
    let (shell, cx) = shell(cx, PHONE);
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    let now = Utc::now().timestamp_millis();
    let parts = (0..40)
        .map(|n| MessagePart::Text {
            id: format!("a:answer:{n}"),
            text: format!("Line {n} of a long answer, long enough to wrap on a phone's width."),
        })
        .collect();
    let entry = |id: &str, role, parts, status| SessionMessageEntry {
        id: id.into(),
        role,
        parts,
        created_at: now,
        device_id: "local".into(),
        status,
        continuation_of: None,
        duration_ms: None,
    };
    let ask = vec![MessagePart::Text { id: "a:ask:t".into(), text: "Write a lot".into() }];
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_transcript(vec![
                entry("a:ask", MessageRole::User, ask, None),
                entry("a:answer", MessageRole::Assistant, parts, Some(MessageStatus::Complete)),
            ]);
            cx.notify();
        });
    });
    let frames = |cx: &mut VisualTestContext| {
        for _ in 0..30 {
            cx.executor().advance_clock(Duration::from_millis(16));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw_and_present(cx));
        }
    };
    frames(cx);
    let above = |shell: &Entity<Shell>, cx: &mut VisualTestContext| {
        shell.update(cx, |shell, cx| shell.transcript.read(cx).distance_from_bottom())
    };
    assert!(above(&shell, cx) <= 0.5, "the chat opens at its end: {}", above(&shell, cx));

    // The keyboard rises: the view is shorter.
    cx.simulate_resize(gpui::size(px(PHONE.0), px(PHONE.1 - 330.0)));
    frames(cx);
    let risen = above(&shell, cx);
    assert!(risen <= 0.5, "with the keyboard up the chat stays at its end: {risen}px above it");

    // Scrolled up by the person, the chat stays where they left it when the keyboard falls.
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: gpui::point(px(200.0), px(300.0)),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(900.0))),
        ..Default::default()
    });
    frames(cx);
    let up = above(&shell, cx);
    cx.simulate_resize(gpui::size(px(PHONE.0), px(PHONE.1)));
    frames(cx);
    let after = above(&shell, cx);
    // The line at the top stays: the view grows downward by what the keyboard frees.
    assert!(
        up > 600.0 && (up - after - 330.0).abs() <= 1.0,
        "a chat scrolled up past what the keyboard frees keeps its top line: {up} -> {after}"
    );
}

// slopty: Slopty's own tests (not in zeron).
//! slopty: live pages in a chat take the pointer: the whole shell drawn headless at the iPad's
//! size, judged by the hit map the window presents, which the platform's hit testing reads.

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

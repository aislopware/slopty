// slopty: Slopty's own tests (not in zeron).
//! slopty: the whole shell drawn under gpui-fast's view retention against the same state drawn
//! from scratch, through what the self-test's `dump` judges (`slopty_kit::retained::stale`): ⌘1
//! opening a chat whose turn runs, and that turn's spinner going on. GPUI's retained check runs
//! on every frame of them too (`Window::set_check_retained`). It also holds the shell window, the
//! fixtures and the reading of the accessibility tree that the chrome's and the sidebar's tests
//! share.
use std::collections::HashMap;

use super::*;
use chrono::TimeDelta;
use gpui::accesskit::{NodeId, Role};
use gpui::{TestAppContext, VisualTestContext};
use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use slopty_view_proto::{Session, SessionStatus};

/// The self-test's window.
const WINDOW: (f32, f32) = (1320.0, 880.0);

/// What the window last painted, sorted.
pub(super) fn painted(window: &Window) -> Vec<String> {
    let mut lines = window.painted_primitives();
    lines.sort_unstable();
    lines
}

/// What a frame drawn from scratch, at the moment the last frame showed, paints.
pub(super) fn from_scratch(window: &mut Window, cx: &mut App) -> Vec<String> {
    window.repeat_frame_time();
    window.refresh();
    window.draw(cx).clear(cx);
    painted(window)
}

/// The lines of sorted `a` that sorted `b` does not hold.
fn only(a: &[String], b: &[String]) -> Vec<String> {
    let mut rest = b.iter().peekable();
    let mut out = Vec::new();
    for line in a {
        while rest.next_if(|other| *other < line).is_some() {}
        if rest.next_if(|other| *other == line).is_none() {
            out.push(line.clone());
        }
    }
    out
}

/// Where the frame shown differs from the same state drawn from scratch, as
/// `slopty_kit::retained::stale` judges it: when two frames from scratch differ, only what both
/// paint alike must be in the frame shown.
pub(super) fn stale(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|window, cx| {
        let shown = painted(window);
        let scratch = from_scratch(window, cx);
        if shown == scratch {
            return None;
        }
        let again = from_scratch(window, cx);
        let (extra, missed) = if again == scratch {
            (only(&shown, &scratch), only(&scratch, &shown))
        } else {
            // In motion: what both frames from scratch paint alike must be in the frame shown.
            let still = only(&scratch, &only(&scratch, &again));
            (Vec::new(), only(&still, &shown))
        };
        let cut = |lines: &[String]| lines.iter().take(12).cloned().collect::<Vec<_>>().join("\n  ");
        (!missed.is_empty() || !extra.is_empty()).then(|| {
            format!(
                "{} painted that a frame from scratch does not:\n  {}\n{} a frame from scratch paints that were not:\n  {}",
                extra.len(),
                cut(&extra),
                missed.len(),
                cut(&missed)
            )
        })
    })
}

/// What two frames from scratch at one moment, the wall clock moving on between them, paint
/// differently: what reads the wall clock where it should read the frame's moment, which no
/// frame drawn again can show as a frame from scratch would.
pub(super) fn wobble(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|window, cx| {
        let first = from_scratch(window, cx);
        std::thread::sleep(std::time::Duration::from_millis(15));
        let second = from_scratch(window, cx);
        (first != second).then(|| {
            let cut = |lines: &[String]| lines.iter().take(8).cloned().collect::<Vec<_>>().join("\n  ");
            let (gone, came) = (only(&first, &second), only(&second, &first));
            format!("drawn 15 ms of the wall clock later at the same moment:\n  {}\nbecame\n  {}", cut(&gone), cut(&came))
        })
    })
}

pub(super) fn turn(chat: &str, now: chrono::DateTime<Utc>) -> Vec<SessionMessageEntry> {
    let entry = |id: &str, role, parts, status| SessionMessageEntry {
        id: id.into(),
        role,
        parts,
        created_at: now.timestamp_millis(),
        device_id: "local".into(),
        status,
        continuation_of: None,
        duration_ms: None,
    };
    let mut entries = Vec::new();
    for n in 0..6 {
        entries.push(entry(
            &format!("{chat}:ask:{n}"),
            MessageRole::User,
            vec![MessagePart::Text { id: format!("{chat}:ask:{n}:t"), text: format!("Question number {n} about the code") }],
            None,
        ));
        entries.push(entry(
            &format!("{chat}:answer:{n}"),
            MessageRole::Assistant,
            vec![MessagePart::Text {
                id: format!("{chat}:answer:{n}:t"),
                text: "An answer long enough to wrap over a few lines of the transcript, so the list scrolls and the view under the titlebar holds a row.\n\nAnd a second paragraph.".into(),
            }],
            Some(MessageStatus::Complete),
        ));
    }
    entries.push(entry(
        &format!("{chat}:ask:last"),
        MessageRole::User,
        vec![MessagePart::Text { id: format!("{chat}:ask:last:t"), text: "slow 30".into() }],
        None,
    ));
    entries.push(entry(
        &format!("{chat}:answer:last"),
        MessageRole::Assistant,
        vec![MessagePart::Text { id: format!("{chat}:answer:last:t"), text: "Working on it".into() }],
        Some(MessageStatus::Streaming),
    ));
    entries
}

/// The screen a test's shell window stands for: its size in points, its scale, whether it is a
/// touch screen's (a phone's pages under 700 pt, an iPad's columns) and whether motion is reduced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Screen {
    pub size: (f32, f32),
    pub scale: f32,
    pub touch: bool,
    pub reduced: bool,
}

/// The self-test's window, motion reduced.
pub(super) const DESKTOP: Screen = Screen { size: WINDOW, scale: 2.0, touch: false, reduced: true };
/// The iPhone the simulator e2e runs (an iPhone 17 Pro): 402 × 874 pt at 3 ×, in motion.
pub(super) const PHONE: Screen = Screen { size: (402.0, 874.0), scale: 3.0, touch: true, reduced: false };

/// A chat of the fixtures: `parent` makes it a subagent of that chat, one that runs inside its
/// parent's agent (and takes no messages) when `in_parent`.
pub(super) fn chat_row(
    id: &str,
    ago: i64,
    parent: Option<(&str, bool)>,
    now: chrono::DateTime<Utc>,
) -> slopty_view_proto::Chat {
    let mut chat = serde_json::json!({
        "id": id, "title": format!("Chat {id}"), "deviceId": "local", "archived": false,
        "createdAt": now - TimeDelta::minutes(ago), "cwd": "/work/app",
        "lastMessageAt": now - TimeDelta::seconds(30), "lastSeenAt": now,
    });
    if let Some((parent, in_parent)) = parent {
        chat["parentChatId"] = parent.into();
        chat["subagent"] = true.into();
        chat["inParent"] = in_parent.into();
    }
    serde_json::from_value(chat).unwrap()
}

/// A chat's session in `status`.
pub(super) fn session_row(id: &str, status: SessionStatus, now: chrono::DateTime<Utc>) -> Session {
    Session {
        last_completed_turn: None,
        chat_id: id.into(),
        device_id: "local".into(),
        status,
        started_at: Some(now - TimeDelta::minutes(1)),
        updated_at: now,
        running_subagents: 0,
        status_since: Some(now - TimeDelta::seconds(20)),
        waiting_on: None,
    }
}

/// A shell window as the self-test's, with two chats, the first of them at work, and no chat
/// open yet.
fn shell(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
    launch(cx, DESKTOP)
}

/// A shell window on `screen`, with two chats, the first of them at work, and no chat open yet.
/// GPUI checks every frame it draws against one from scratch, and panics on a stale one.
pub(super) fn launch(cx: &mut TestAppContext, screen: Screen) -> (Entity<Shell>, &mut VisualTestContext) {
    crate::motion::forget_for_test(); // slopty: no hover from the last seed
    let dir = tempfile::tempdir().unwrap();
    compact::force_touch(screen.touch);
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        settings::init(UiSettings::default(), dir.path(), cx);
        crate::composer::init(cx);
        apply_keymap(cx, &KeymapConfig::default());
        cx.set_reduce_motion(screen.reduced);
    });
    let path = dir.path().to_path_buf();
    let (shell, cx) = cx.add_window_view(move |_, cx| {
        let state = cx.new(|_| AppState::new());
        let mut shell = Shell::new(
            state,
            EngineBootConfig { data_dir: path.clone(), default_harness: slopty_view_proto::HarnessId::MOCK },
            cx,
        );
        shell.debug_gate = Some(GatePhase::Ready);
        shell.splash = SplashPhase::Gone;
        let now = Utc::now();
        shell.state.update(cx, |state, _| {
            state.connection = slopty_view_proto::view::ConnectionStatus::Ready;
            state.workspace_scope = Some(WorkspaceScope::Local);
            state.local_device_id = Some("local".into());
            state.chats = vec![chat_row("a", 1, None, now), chat_row("b", 2, None, now)];
            state.apply_sessions(vec![
                session_row("a", SessionStatus::Working, now),
                session_row("b", SessionStatus::Done, now),
            ]);
        });
        shell
    });
    std::mem::forget(dir);
    // slopty: GPUI checks every frame that draws a view again against one drawn from scratch at
    // its moment, and panics naming the view a frame drew stale.
    cx.update(|window, _| window.set_check_retained(Some(gpui::RetainedCheck::Panic)));
    cx.simulate_scale_factor_change(screen.scale);
    cx.simulate_resize(gpui::size(px(screen.size.0), px(screen.size.1)));
    cx.run_until_parked();
    (shell, cx)
}

/// A node of the last frame's accessibility tree.
#[derive(Debug, Clone)]
pub(super) struct Node {
    pub role: Role,
    pub label: String,
    pub description: Option<String>,
    pub expanded: Option<bool>,
    pub disabled: bool,
    pub bounds: gpui::Bounds<Pixels>,
}

pub(super) fn tree(cx: &mut VisualTestContext) -> HashMap<NodeId, Node> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.set_a11y_active(true);
        window.refresh();
        window.draw(cx).clear(cx);
        let Some(update) = window.a11y_tree() else { return HashMap::new() };
        let scale = f64::from(window.scale_factor());
        update
            .nodes
            .iter()
            .map(|(id, node)| {
                let r = node.bounds().unwrap_or_default();
                let at = |v: f64| px((v / scale) as f32);
                (
                    *id,
                    Node {
                        role: node.role(),
                        label: node.label().unwrap_or_default().to_owned(),
                        description: node.description().map(str::to_owned),
                        expanded: node.is_expanded(),
                        disabled: node.is_disabled(),
                        bounds: gpui::Bounds::from_corners(
                            gpui::point(at(r.x0), at(r.y0)),
                            gpui::point(at(r.x1), at(r.y1)),
                        ),
                    },
                )
            })
            .collect()
    })
}

pub(super) fn find(nodes: &HashMap<NodeId, Node>, role: Role, label: &str) -> Option<Node> {
    nodes.values().find(|node| node.role == role && node.label == label).cloned()
}

pub(super) fn find_starting(nodes: &HashMap<NodeId, Node>, role: Role, label: &str) -> Option<Node> {
    nodes.values().find(|node| node.role == role && node.label.starts_with(label)).cloned()
}

pub(super) fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

/// A project on the local device.
pub(super) fn space(id: &str) -> slopty_view_proto::Space {
    serde_json::from_value(serde_json::json!({
        "id": id, "deviceId": "local", "path": format!("/work/{id}"), "createdAt": Utc::now(),
    }))
    .unwrap()
}

/// The local device, as the server lists it.
pub(super) fn device() -> slopty_view_proto::Device {
    serde_json::from_value(serde_json::json!({
        "id": "local", "name": "Studio", "platform": "macos", "createdAt": Utc::now(),
    }))
    .unwrap()
}

/// Frames delivered `dt` apart, as the display asks for them, until none is asked for or `cap`
/// were drawn; how many were.
pub(super) fn frames(cx: &mut VisualTestContext, dt: std::time::Duration, cap: usize) -> usize {
    cx.run_until_parked();
    for drawn in 0..cap {
        cx.executor().advance_clock(dt);
        if cx.update(|window, cx| window.simulate_next_frame(cx)) == 0 {
            cx.run_until_parked();
            return drawn;
        }
        cx.run_until_parked();
    }
    cap
}

/// ⌘1 opens the chat at work; its transcript then arrives as the engine sends it. Every frame
/// the window shows on the way paints what a frame from scratch paints, the band under the
/// titlebar included.
#[gpui::test]
fn a_chat_opened_with_cmd_1_paints_as_from_scratch(cx: &mut TestAppContext) {
    let (shell, cx) = shell(cx);
    assert_eq!(stale(cx), None, "the window before the chat opens");
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    assert!(shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.as_deref() == Some("a")));
    assert_eq!(stale(cx), None, "the chat just opened, its transcript not yet in");
    let now = Utc::now();
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_transcript(turn("a", now));
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert_eq!(stale(cx), None, "the transcript in");
    // The answer streams in, the list following it down frame after frame, as a scroll layer
    // is promoted for.
    for step in 0..12 {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                let mut entries = turn("a", now);
                if let Some(MessagePart::Text { text, .. }) = entries.last_mut().and_then(|e| e.parts.first_mut()) {
                    for n in 0..=step {
                        text.push_str(&format!("\n\nStreaming paragraph {n}, which says a little more each frame."));
                    }
                }
                state.apply_transcript(entries);
                cx.notify();
            });
        });
        cx.executor().advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
        if step % 3 == 2 {
            assert_eq!(stale(cx), None, "streaming, step {step}");
        }
    }
    for step in 0..4 {
        cx.executor().advance_clock(std::time::Duration::from_millis(250));
        cx.run_until_parked();
        assert_eq!(stale(cx), None, "the turn's spinner going on, step {step}");
    }
}

/// Another chat opened with ⌘2 after the first one's answer streamed under a scroll layer,
/// then ⌘1 back to it: each frame shown paints what a frame from scratch paints.
#[gpui::test]
fn chats_switched_after_a_stream_paint_as_from_scratch(cx: &mut TestAppContext) {
    let (shell, cx) = shell(cx);
    let now = Utc::now();
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    for step in 0..10 {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                let mut entries = turn("a", now);
                if let Some(MessagePart::Text { text, .. }) = entries.last_mut().and_then(|e| e.parts.first_mut()) {
                    for n in 0..=step {
                        text.push_str(&format!("\n\nStreaming paragraph {n}."));
                    }
                }
                state.apply_transcript(entries);
                cx.notify();
            });
        });
        cx.executor().advance_clock(std::time::Duration::from_millis(16));
        cx.run_until_parked();
    }
    for (keys, chat) in [("cmd-2", "b"), ("cmd-1", "a")] {
        cx.simulate_keystrokes(keys);
        cx.run_until_parked();
        assert_eq!(stale(cx), None, "{keys} just pressed");
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.apply_transcript(turn(chat, now));
                cx.notify();
            });
        });
        cx.run_until_parked();
        assert_eq!(stale(cx), None, "{keys}, its transcript in");
    }
}

/// A chat's row says its status to a screen reader: the working chat's row holds a Status node
/// that says Working, and an idle row says nothing.
#[gpui::test]
fn a_rows_status_is_a_node_of_the_tree(cx: &mut TestAppContext) {
    let (_shell, cx) = shell(cx);
    let statuses = cx.update(|window, cx| {
        window.set_a11y_active(true);
        window.refresh();
        window.draw(cx).clear(cx);
        let update = window.a11y_tree().expect("a tree");
        update
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == gpui::accesskit::Role::Status)
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect::<Vec<_>>()
    });
    assert!(statuses.iter().any(|l| l == "Working"), "the working row: {statuses:?}");
    assert_eq!(statuses, ["Working"], "an idle row says nothing: {statuses:?}");
}

/// The boot landing happens once, at the first chats frame: a first frame with no chat leaves
/// the new-session canvas, and a chat that arrives after (started from the CLI or another
/// device) does not open by itself.
#[gpui::test]
fn a_chat_arriving_after_an_empty_first_frame_does_not_open_by_itself(cx: &mut TestAppContext) {
    let (shell, cx) = shell(cx);
    let chat: slopty_view_proto::Chat = serde_json::from_value(serde_json::json!({
        "id": "later", "title": "Started elsewhere", "deviceId": "local", "archived": false,
        "createdAt": Utc::now(), "cwd": "/work/app", "lastMessageAt": Utc::now(),
    }))
    .unwrap();
    for (step, chats) in [("the first frame, empty", Vec::new()), ("a chat arrives", vec![chat])] {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.apply_chats(chats);
                cx.notify();
            });
        });
        cx.run_until_parked();
        let selected = shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone());
        assert_eq!(selected, None, "{step}");
    }
}

/// A phone's session, `screen`, its subagent's timeline opened and left by the inspector's
/// way back to its parent's, as the simulator's e2e takes it. Every frame on the way, with the
/// wall clock moving on between the frame and the check as it does between a frame and the
/// self-test's dump, paints what a frame from scratch at its moment paints.
fn back_from_a_subagent(cx: &mut TestAppContext, screen: Screen) {
    let (shell, cx) = launch(cx, screen);
    let now = Utc::now();
    let mut done = turn("b", now);
    if let Some(last) = done.last_mut() {
        last.status = Some(MessageStatus::Complete);
    }
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, _| state.chats.push(chat_row("look", 0, Some(("b", true)), now)));
        shell.open_chat("b".into(), cx);
        shell.state.update(cx, |state, cx| {
            state.apply_transcript(done.clone());
            cx.notify();
        });
    });
    frames(cx, std::time::Duration::from_millis(16), 120);
    assert!(shell.read_with(cx, |shell, _| shell.compact()), "the phone's own layout");
    assert_eq!(stale(cx), None, "the parent's session");
    shell.update(cx, |shell, cx| shell.open_chat("look".into(), cx));
    frames(cx, std::time::Duration::from_millis(16), 120);
    let selected = shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone());
    assert_eq!(selected.as_deref(), Some("look"), "the subagent's timeline");
    assert_eq!(stale(cx), None, "the subagent's run");
    shell.update(cx, |shell, cx| shell.open_chat("b".into(), cx));
    for step in 0..40 {
        cx.executor().advance_clock(std::time::Duration::from_millis(16));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(12));
        assert_eq!(stale(cx), None, "frame {step} back from the subagent");
        assert_eq!(wobble(cx), None, "frame {step} back from the subagent");
    }
}

/// The e2e's own case: the phone under Reduce Motion, as the self-test runs it.
#[gpui::test]
fn a_phones_way_back_from_a_subagent_paints_as_from_scratch(cx: &mut TestAppContext) {
    back_from_a_subagent(cx, Screen { reduced: true, ..PHONE });
}

/// The same in motion: what moves on the way back moves at the frame's moment, not the wall
/// clock's.
#[gpui::test]
fn a_phones_way_back_in_motion_paints_as_from_scratch(cx: &mut TestAppContext) {
    back_from_a_subagent(cx, PHONE);
}

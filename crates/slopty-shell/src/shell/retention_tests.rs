// slopty: Slopty's own tests (not in zeron).
//! slopty: the whole shell drawn under gpui-fast's view retention against the same state drawn
//! from scratch, through what the self-test's `dump` judges (`slopty_kit::retained::stale`): ⌘1
//! opening a chat whose turn runs, and that turn's spinner going on. GPUI's retained check runs
//! on every frame of them too (`Window::set_check_retained`).
use super::*;
use chrono::TimeDelta;
use gpui::{TestAppContext, VisualTestContext};
use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use slopty_view_proto::{Session, SessionStatus};

/// The self-test's window.
const WINDOW: (f32, f32) = (1320.0, 880.0);

/// What the window last painted, sorted.
fn painted(window: &Window) -> Vec<String> {
    let mut lines = window.painted_primitives();
    lines.sort_unstable();
    lines
}

/// What a frame drawn from scratch, at the moment the last frame showed, paints.
fn from_scratch(window: &mut Window, cx: &mut App) -> Vec<String> {
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
fn stale(cx: &mut VisualTestContext) -> Option<String> {
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

fn turn(chat: &str, now: chrono::DateTime<Utc>) -> Vec<SessionMessageEntry> {
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

/// A shell window as the self-test's, with two chats, the first of them at work, and no chat
/// open yet.
fn shell(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
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
        let mut shell = Shell::new(
            state,
            EngineBootConfig { data_dir: path.clone(), default_harness: slopty_view_proto::HarnessId::MOCK },
            cx,
        );
        shell.debug_gate = Some(GatePhase::Ready);
        shell.splash = SplashPhase::Gone;
        let now = Utc::now();
        let chat = |id: &str, ago: i64| -> slopty_view_proto::Chat {
            serde_json::from_value(serde_json::json!({
                "id": id, "title": format!("Chat {id}"), "deviceId": "local", "archived": false,
                "createdAt": now - TimeDelta::minutes(ago), "cwd": "/work/app",
                "lastMessageAt": now - TimeDelta::seconds(30), "lastSeenAt": now,
            }))
            .unwrap()
        };
        let session = |id: &str, status| Session {
            last_completed_turn: None,
            chat_id: id.into(),
            device_id: "local".into(),
            status,
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
            state.chats = vec![chat("a", 1), chat("b", 2)];
            state.apply_sessions(vec![session("a", SessionStatus::Working), session("b", SessionStatus::Done)]);
        });
        shell
    });
    std::mem::forget(dir);
    // slopty: GPUI checks every frame that draws a view again against one drawn from scratch at
    // its moment, and panics naming the view a frame drew stale.
    cx.update(|window, _| window.set_check_retained(Some(gpui::RetainedCheck::Panic)));
    cx.simulate_resize(gpui::size(px(WINDOW.0), px(WINDOW.1)));
    cx.run_until_parked();
    (shell, cx)
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

/// A message queued under a chat pinned to its end raises the stack under the transcript, and
/// its delivery lowers it again: the transcript's end moves with the stack in the frame that
/// shows it, as a frame from scratch draws it, with no turn of the person's own running.
#[gpui::test]
fn a_queue_that_comes_and_goes_moves_the_transcript_as_from_scratch(cx: &mut TestAppContext) {
    let (shell, cx) = shell(cx);
    cx.simulate_keystrokes("cmd-2");
    cx.run_until_parked();
    let now = Utc::now();
    let mut done = turn("b", now);
    if let Some(last) = done.last_mut() {
        last.status = Some(MessageStatus::Complete);
    }
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_transcript(done.clone());
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert_eq!(stale(cx), None, "the transcript in");
    for (step, queue) in [
        ("queued", vec![slopty_view_doc::QueuedMessage::new("q1", "Say more", "local")]),
        ("delivered", Vec::new()),
    ] {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.queue = queue;
                cx.notify();
            });
        });
        cx.run_until_parked();
        assert_eq!(stale(cx), None, "{step}");
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

/// A draft typed into the composer while a turn runs, in a chat that scrolls and in one that
/// does not, paints as from scratch after every key.
#[gpui::test]
fn a_draft_typed_mid_turn_paints_as_from_scratch(cx: &mut TestAppContext) {
    let (shell, cx) = shell(cx);
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    let now = Utc::now();
    for (case, entries) in [("short", turn("a", now).split_off(12)), ("long", turn("a", now))] {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.apply_transcript(entries.clone());
                cx.notify();
            });
            shell.composer.update(cx, |composer, cx| composer.input.update(cx, |input, cx| input.set_text("", cx)));
        });
        cx.run_until_parked();
        assert_eq!(stale(cx), None, "{case}: the transcript in");
        let input = shell.read_with(cx, |shell, cx| shell.composer.read(cx).input.clone());
        cx.update(|window, cx| window.focus(&input.read(cx).focus_handle(cx), cx));
        cx.run_until_parked();
        assert_eq!(stale(cx), None, "{case}: the composer focused");
        for ch in "Say more".chars() {
            cx.simulate_input(&ch.to_string());
            cx.run_until_parked();
            assert_eq!(stale(cx), None, "{case}: after {ch:?}");
        }
    }
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

/// The composer's caret blinks as state its driver flips: a frame drawn again past a
/// half-period of the wall clock, with no flip between, shows the caret as the frame it
/// replaces did.
#[gpui::test]
fn a_caret_drawn_again_later_shows_as_it_did(cx: &mut TestAppContext) {
    let (shell, cx) = shell(cx);
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    let input = shell.read_with(cx, |shell, cx| shell.composer.read(cx).input.clone());
    cx.update(|window, cx| {
        window.activate_window();
        window.focus(&input.read(cx).focus_handle(cx), cx);
    });
    cx.run_until_parked();
    let shown = cx.update(|window, cx| {
        window.is_window_active() && input.read(cx).focus_handle(cx).is_focused(window)
    });
    assert!(shown, "the caret is drawn: the window active, the composer focused");
    assert_eq!(stale(cx), None, "the caret on");
    std::thread::sleep(std::time::Duration::from_millis(crate::composer::CARET_BLINK_MS + 50));
    assert_eq!(stale(cx), None, "past a half-period of the wall clock");
}

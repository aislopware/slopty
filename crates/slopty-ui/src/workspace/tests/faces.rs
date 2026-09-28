//! An agent terminal's conversation face in the workspace: the toggle, what it follows, what
//! its composer types into the PTY, and its answers to a held prompt.

use slopty_core::WallMs;
use slopty_proto::conversation::{ConversationRequest, PermissionEvent, Settled, Verdict};

use super::*;
use crate::conversation::{ConversationView, fixtures};

/// A shell of this client's on `fake`'s worker with Claude Code working in it, focused.
fn agent_tile(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &mut Fake,
) -> (TileRef, SessionId) {
    let session = SessionId::new();
    let tile = opens(view, cx, fake, session, fake.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    fake.drain();
    (tile, session)
}

/// What the workspace asked of the worker about conversations.
fn requests(fake: &mut Fake) -> Vec<ConversationRequest> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Conversation(req) => Some(req),
            _ => None,
        })
        .collect()
}

fn face_shown(view: &Entity<WorkspaceView>, cx: &VisualTestContext, session: SessionId) -> bool {
    view.read_with(cx, |v, _| v.face_shown(session))
}

/// ⌘J swaps the tile between the TUI and the face over the same session: showing the face
/// follows the conversation and gives the composer the keyboard, hiding it unfollows and gives
/// the terminal the keyboard back, and a draft and the place in the list survive the round.
#[gpui::test]
fn the_face_toggles_over_the_same_session_and_keeps_its_draft(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (tile, session) = agent_tile(&view, cx, &mut studio);
    assert!(!face_shown(&view, cx, session), "the TUI is the default on a Mac");

    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    assert!(face_shown(&view, cx, session));
    assert_eq!(requests(&mut studio), [ConversationRequest::Follow { session }]);
    view.update_in(cx, |v, _w, cx| {
        for event in fixtures::events("edit") {
            v.conversation_event(session, event, cx);
        }
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("composer").is_some(), "the face is drawn in the tile");
    let face = view.read_with(cx, |v, _| v.conversation(session).cloned()).expect("a face");
    assert!(!face.read_with(cx, |f, _| f.rows().is_empty()), "the conversation is in its list");
    cx.simulate_input("half a thought");
    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    assert!(!face_shown(&view, cx, session));
    assert_eq!(requests(&mut studio), [ConversationRequest::Unfollow { session }]);
    assert!(terminal_focused(&view, cx, session), "the TUI takes the keyboard back");
    assert!(cx.debug_bounds("composer").is_none());

    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    assert_eq!(requests(&mut studio), [ConversationRequest::Follow { session }]);
    assert_eq!(face.read_with(cx, ConversationView::draft), "half a thought", "the draft waited");
    assert_eq!(focused(&view, cx), Some(tile));
}

/// The composer types into the agent's own PTY as a person would: a message as one bracketed
/// paste and, a moment later, Enter; a slash command as typed text. Esc stops the turn.
#[gpui::test]
fn the_composer_types_into_the_same_pty(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (_tile, session) = agent_tile(&view, cx, &mut studio);
    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    studio.drain();
    let sent = |studio: &mut Fake| -> Vec<TermRequest> {
        studio
            .drain()
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Term { session: s, req } if s == session => Some(req),
                _ => None,
            })
            .collect()
    };

    cx.simulate_input("Fix the flaky test");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(sent(&mut studio), [TermRequest::Paste("Fix the flaky test".into())]);
    cx.executor().advance_clock(crate::conversation::composer::SUBMIT_PAUSE);
    cx.run_until_parked();
    let enter = sent(&mut studio);
    assert!(
        matches!(enter.as_slice(), [TermRequest::Key(k)] if k.code == slopty_proto::input::KeyCode::Enter),
        "Enter after the pause: {enter:?}"
    );

    cx.simulate_input("/compact");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(sent(&mut studio), [TermRequest::Raw(b"/compact".to_vec())]);
    cx.executor().advance_clock(crate::conversation::composer::SUBMIT_PAUSE);
    cx.run_until_parked();
    studio.drain();

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let esc = sent(&mut studio);
    assert!(
        matches!(esc.as_slice(), [TermRequest::Key(k)] if k.code == slopty_proto::input::KeyCode::Escape),
        "Esc stops the working agent: {esc:?}"
    );
}

/// A held prompt takes the composer's place and says what Always would grant;
/// Allow once answers it once, however often it is pressed, and the worker's word that it
/// settled takes the card away.
#[gpui::test]
fn a_held_prompt_is_answered_once(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (_tile, session) = agent_tile(&view, cx, &mut studio);
    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    studio.drain();
    view.update_in(cx, |v, _w, cx| {
        let asked = fixtures::bash_prompt(session, 7);
        v.permission_event(PermissionEvent::Asked(Box::new(asked)), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("approval").is_some(), "the card is up");
    assert!(cx.debug_bounds("composer").is_none(), "in the composer's place");
    assert!(cx.debug_bounds("always-scope").is_some(), "what Always grants, above the buttons");

    for _ in 0..2 {
        let at = cx.debug_bounds("allow-once").expect("allow once").center();
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    }
    assert_eq!(
        requests(&mut studio),
        [ConversationRequest::Answer { session, ask: 7, verdict: Verdict::Allow }]
    );
    let me = studio.me;
    view.update_in(cx, |v, _w, cx| {
        let outcome = Settled::Answered { verdict: Verdict::Allow, by: me };
        v.permission_event(PermissionEvent::Settled { session, ask: 7, outcome }, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("approval").is_none());
    assert!(cx.debug_bounds("composer").is_some());
}

/// A prompt the worker hands back to the TUI says so and offers the terminal.
#[gpui::test]
fn a_released_prompt_offers_the_terminal(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (_tile, session) = agent_tile(&view, cx, &mut studio);
    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| {
        let asked = fixtures::bash_prompt(session, 3);
        v.permission_event(PermissionEvent::Asked(Box::new(asked)), cx);
        let outcome = Settled::Released;
        v.permission_event(PermissionEvent::Settled { session, ask: 3, outcome }, cx);
    });
    cx.run_until_parked();
    let at = cx.debug_bounds("show-terminal").expect("the way to the TUI").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert!(!face_shown(&view, cx, session));
    assert!(terminal_focused(&view, cx, session));
}

/// Closing the tile unfollows its conversation; so does the agent leaving the terminal.
#[gpui::test]
fn closing_the_tile_or_the_agent_leaving_unfollows(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (tile, session) = agent_tile(&view, cx, &mut studio);
    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    assert_eq!(requests(&mut studio), [ConversationRequest::Follow { session }]);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::None, ..blocked(session) }, cx);
    });
    cx.run_until_parked();
    assert_eq!(requests(&mut studio), [ConversationRequest::Unfollow { session }]);
    assert!(terminal_focused(&view, cx, session), "the keyboard goes back to the TUI");

    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
    });
    cx.run_until_parked();
    assert_eq!(requests(&mut studio), [ConversationRequest::Follow { session }], "the pick stuck");
    view.update_in(cx, |v, _w, cx| {
        let key = studio.key;
        v.apply_sync(
            key,
            ItemSync::Delta { version: 2, by: ClientId::new(), op: ItemOp::Remove(tile.item) },
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(requests(&mut studio), [ConversationRequest::Unfollow { session }]);
}

/// On a phone-width layout an agent's tile shows its conversation until the person picks
/// the TUI, and the pick stays.
#[gpui::test]
fn a_phone_shows_the_face_first(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    let mut studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let _tile = opens(&view, cx, &studio, session, studio.me, 1);
    studio.drain();
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
    });
    cx.run_until_parked();
    assert!(face_shown(&view, cx, session));
    assert_eq!(requests(&mut studio), [ConversationRequest::Follow { session }]);
    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    assert!(!face_shown(&view, cx, session), "the person picked the TUI");
}

/// On a phone the title of an agent's tile reads whole beside its pill. The pill says the
/// state alone ("Needs approval") on any screen, since what the agent asks is the navigator's
/// line and the pointer's, and a screen reader still hears all of it. Nothing runs past the
/// header.
#[gpui::test]
fn a_phone_header_keeps_its_title_and_the_pill_gives_way(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    cx.run_until_parked();
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    let asks = AgentEvent {
        status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
        detail: Some("$ touch a-file-with-a-rather-long-name-for-a-phone.txt".into()),
        ..blocked(session)
    };
    view.update_in(cx, |v, _w, cx| v.agent_event(asks, cx));
    cx.run_until_parked();
    let widths = |cx: &mut VisualTestContext| {
        let name = cx.debug_bounds(selector("name", tile.item)).expect("the title");
        let pill = cx.debug_bounds(selector("agent", tile.item)).expect("the pill");
        let header = cx.debug_bounds(selector("title", tile.item)).expect("the header");
        assert!(pill.left() >= name.right(), "side by side: {name:?} {pill:?}");
        assert!(pill.right() <= header.right(), "inside the header: {pill:?} {header:?}");
        (f32::from(name.size.width), f32::from(pill.size.width))
    };
    let (title, desk) = widths(cx);

    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    let (phone_title, word) = widths(cx);
    assert!((phone_title - title).abs() < 0.5, "the whole title: {phone_title} of {title}");
    assert!((word - desk).abs() < 0.5, "the state alone on both: {word} against {desk}");
    let nodes = tree(cx);
    let full = "Needs approval: $ touch a-file-with-a-rather-long-name-for-a-phone.txt";
    assert!(nodes.iter().any(|n| n.label.as_deref() == Some(full)), "all of it, said");
    let brief = AgentEvent {
        status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
        detail: Some("$ ls".into()),
        ..blocked(session)
    };
    view.update_in(cx, |v, _w, cx| v.agent_event(brief, cx));
    cx.run_until_parked();
    let (_, other) = widths(cx);
    assert!((other - word).abs() < 0.5, "the word, not the ask: {other} against {word}");
}

/// While the face shows the agent mid-turn (a block still streaming, a call in flight), its
/// tile is marked and the navigator lists it as working though the worker's hook still says
/// idle, and neither of its lines says "Idle"; once the block settles the mark follows the hook
/// again. The face projects the transcript; it drives nothing.
#[gpui::test]
fn a_face_mid_turn_marks_its_tile_working_while_the_hook_lags(cx: &mut TestAppContext) {
    use std::time::SystemTime;

    use slopty_proto::conversation::{
        BashDetail, Body, Change, Clipped, ConversationEvent, Entry, Live, LiveId, LiveKind,
        ShellStatus, ThreadId, ToolCall, ToolDetail,
    };

    use crate::icons::Status;

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (tile, session) = agent_tile(&view, cx, &mut studio);
    cx.simulate_keystrokes("cmd-j");
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Idle, ..blocked(session) }, cx);
    });
    cx.run_until_parked();
    let mark = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| v.item(tile).and_then(|item| v.tile_status(tile, item, cx)))
    };
    let listed = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| v.working(cx).iter().any(|w| w.session == session))
    };
    assert_eq!(mark(cx), Some(Status::Idle), "the hook's word");

    let id = LiveId { turn: "t1".into(), step: 0, block: 0 };
    let start = Live::Start { thread: ThreadId::Main, id: id.clone(), kind: LiveKind::Text };
    view.update_in(cx, |v, _w, cx| {
        v.conversation_event(session, ConversationEvent::Live(vec![start]), cx);
    });
    cx.run_until_parked();
    assert_eq!(mark(cx), Some(Status::Working), "the face knows better");
    assert!(listed(cx), "and the navigator lists it under Working");
    // The row and its agent line under Working: what the face says, never the hook's "Idle".
    let lines = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| {
            let meta = v.item(tile).map(|item| v.tile_meta(item, SystemTime::now(), cx).0);
            let words = v
                .working(cx)
                .into_iter()
                .find(|w| w.session == session)
                .map(|at| v.working_words(at, cx));
            (meta.unwrap_or_default(), words.unwrap_or_default())
        })
    };
    let (meta, words) = lines(cx);
    assert!(!meta.contains("Idle") && !words.contains("Idle"), "{meta:?} / {words:?}");

    view.update_in(cx, |v, _w, cx| {
        v.conversation_event(session, ConversationEvent::Live(vec![Live::Clear { id }]), cx);
    });
    cx.run_until_parked();
    assert_eq!(mark(cx), Some(Status::Idle), "settled: the hook's word again");
    assert!(!listed(cx), "and no longer working");

    // A call whose input is still streaming has no entry yet: the lines name it as its live
    // row does.
    let streaming = LiveId { turn: "t1".into(), step: 1, block: 0 };
    let kind = LiveKind::Tool { id: "toolu_0".into(), name: "Bash".into() };
    let events = vec![
        Live::Start { thread: ThreadId::Main, id: streaming.clone(), kind },
        Live::Append { id: streaming.clone(), text: r#"{"command": "echo hi""#.into() },
    ];
    view.update_in(cx, |v, _w, cx| {
        v.conversation_event(session, ConversationEvent::Live(events), cx);
    });
    cx.run_until_parked();
    let (meta, words) = lines(cx);
    assert!(meta.contains("echo hi") && words.contains("echo hi"), "{meta:?} / {words:?}");
    view.update_in(cx, |v, _w, cx| {
        let clear = Live::Clear { id: streaming };
        v.conversation_event(session, ConversationEvent::Live(vec![clear]), cx);
    });
    cx.run_until_parked();

    // A call in flight with no answer before it: the face's summary names the call.
    let call = ToolCall {
        name: "Bash".into(),
        detail: ToolDetail::Bash(BashDetail {
            command: Clipped { text: "echo hi".into(), lines: 1, chars: 7, full: None },
            description: None,
            background: false,
            task_id: None,
            status: ShellStatus::Running,
            exit_code: None,
            stdout: None,
            stderr: None,
            output_file: None,
            finished_ms: None,
        }),
        result: None,
    };
    let entry =
        Entry { id: "toolu_1".into(), at_ms: WallMs::ZERO, body: Body::Tool(Box::new(call)) };
    let upsert = Change::Upsert { thread: ThreadId::Main, entry };
    view.update_in(cx, |v, _w, cx| {
        v.conversation_event(session, ConversationEvent::Changes(vec![upsert]), cx);
    });
    cx.run_until_parked();
    assert_eq!(mark(cx), Some(Status::Working), "a call in flight");
    let (meta, words) = lines(cx);
    assert!(!meta.contains("Idle") && !words.contains("Idle"), "{meta:?} / {words:?}");
    assert!(meta.contains("echo hi") && words.contains("echo hi"), "{meta:?} / {words:?}");
}

/// While the face shows an approval, the card is the tile's statement: the header wears no
/// pill saying the same thing above it. Over the TUI the pill is back.
#[gpui::test]
fn the_approval_card_is_said_once(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (tile, session) = agent_tile(&view, cx, &mut studio);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(blocked(session), cx);
        v.show_face(session, true, cx);
    });
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| {
        let asked = fixtures::bash_prompt(session, 7);
        v.permission_event(PermissionEvent::Asked(Box::new(asked)), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("approval").is_some(), "the card is up");
    assert!(cx.debug_bounds(selector("agent", tile.item)).is_none(), "no pill over it");
    view.update_in(cx, |v, _w, cx| v.show_face(session, false, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("agent", tile.item)).is_some(), "the TUI's header says it");
}

/// The status bar counts the agents at work whose tiles are off screen: one in view says so
/// in its own header.
#[gpui::test]
fn the_status_bar_counts_only_agents_out_of_view(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (tile, _session) = agent_tile(&view, cx, &mut studio);
    assert!(cx.debug_bounds("status-agents").is_none(), "its tile is on screen");
    let far = (2..6)
        .map(|n| opens(&view, cx, &studio, SessionId::new(), studio.me, n))
        .last()
        .expect("a shell");
    view.update_in(cx, |v, _w, cx| v.focus_tile(far, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("item", tile.item)).is_none(), "scrolled away");
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-agents").is_some(), "counted once it is out of view");
}

/// An agent that has not titled itself is named by its session's first prompt once its face
/// has read the conversation, rather than "Claude Code" beside "Claude Code 2".
#[gpui::test]
fn an_untitled_agent_is_named_by_its_first_prompt(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (_tile, session) = agent_tile(&view, cx, &mut studio);
    let title =
        |cx: &mut VisualTestContext| view.read_with(cx, |v, cx| v.terminal_title(session, cx));
    assert_eq!(title(cx), "Claude Code", "nothing read yet");
    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| {
        for event in fixtures::events("edit") {
            v.conversation_event(session, event, cx);
        }
    });
    cx.run_until_parked();
    let prompt = view
        .read_with(cx, |v, cx| v.conversation(session).and_then(|f| f.read(cx).first_prompt()))
        .expect("the recorded session opens on a prompt");
    assert_eq!(title(cx), prompt);
}

/// Two untitled agents read alike until one's first prompt names it; the other then loses its
/// number, though nothing but the face's news changed.
#[gpui::test]
fn a_first_prompt_renumbers_the_agents_that_read_alike(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (first, one) = agent_tile(&view, cx, &mut studio);
    let two = SessionId::new();
    let second = opens(&view, cx, &studio, two, studio.me, 2);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(two) }, cx);
        v.focus_tile(first, cx);
    });
    cx.run_until_parked();
    let titles = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| [first, second].map(|t| v.tile_title(v.item(t).unwrap(), cx)))
    };
    assert_eq!(titles(cx), ["Claude Code", "Claude Code 2"]);
    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    let first_prompt = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| v.conversation(one).and_then(|f| f.read(cx).first_prompt()))
    };
    for event in fixtures::events("edit") {
        view.update_in(cx, |v, _w, cx| v.conversation_event(one, event, cx));
        cx.run_until_parked();
        if let Some(prompt) = first_prompt(cx) {
            assert_eq!(titles(cx), [prompt, "Claude Code".to_owned()]);
            return;
        }
    }
    panic!("the recorded session has a prompt");
}

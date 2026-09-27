//! An agent terminal's conversation face in the workspace: the toggle, what it follows, what
//! its composer types into the PTY, and its answers to a held prompt.

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

/// A held prompt takes the composer's place; Allow once answers it once, however often it is
/// pressed, and the worker's word that it settled takes the card away.
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
    assert!(cx.debug_bounds("always-grants").is_some(), "Always says what it grants");

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

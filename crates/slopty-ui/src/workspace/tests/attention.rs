//! What notifies on a pocketed phone, against a notifier that shows nothing: the decisions on
//! their own, then a headless workspace's look and a tapped note's way back to its tile.

use std::sync::Arc;

use gpui::{Entity, TestAppContext, VisualTestContext, px, size};
use slopty_core::{ClientId, WallMs};
use slopty_platform::notify::Memory;
use slopty_proto::ClientMsg;
use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, AgentStatus, BlockReason};
use slopty_proto::conversation::{
    Clipped, ConversationRequest, PermissionEvent, PermissionPrompt, ToolDetail,
};
use slopty_proto::handshake::HelloAck;
use slopty_proto::items::{Item, ItemOp, ItemSync};
use slopty_proto::server::{Os, WorkerCaps};
use slopty_proto::terminal::{SessionState, SessionSummary};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use super::*;
use crate::screen::ScreenFactory;
use crate::workspace::{WorkerLink, WorkspaceEvent};

fn route(seed: u128) -> Route {
    Route { worker: WorkerKey::new(seed), item: Some(ItemId::new()), session: SessionId::new() }
}

fn asking(route: Route, body: &str) -> Asking {
    Asking { route, title: "api".into(), body: body.into(), approval: None, answered: None }
}

fn attention() -> (Attention, Rc<Memory>) {
    let memory = Rc::new(Memory::default());
    (Attention::new(Rc::<Memory>::clone(&memory)), memory)
}

#[test]
fn an_agent_that_starts_to_wait_notifies_only_while_the_app_is_away() {
    let (mut attention, memory) = attention();
    let (a, b) = (route(1), route(2));
    attention.look(&Look { asking: vec![asking(a, "Run cargo test")], unread: 1 });
    assert!(memory.posted().is_empty(), "in front, the inbox says it");

    attention.set_active(false);
    attention.look(&Look { asking: vec![asking(a, "Run cargo test")], unread: 1 });
    assert!(
        memory.posted().is_empty(),
        "an agent seen waiting before the app left says nothing new"
    );

    attention.look(&Look {
        asking: vec![asking(a, "Run cargo test"), asking(b, "Asks: which one?")],
        unread: 2,
    });
    let posted = memory.posted();
    assert_eq!(posted.len(), 1, "only the agent that started to wait: {posted:?}");
    let note = &posted[0];
    assert_eq!(note.id, b.session.to_string(), "one note per tile, named by its session");
    assert_eq!(
        (note.title.as_str(), note.body.as_str()),
        ("api", "Asks: which one?"),
        "the tile and the ask"
    );
    assert_eq!(
        Route::of_tap(&Tap { id: note.id.clone(), info: note.info.clone(), action: None }),
        Some(b),
        "a tap carries the route"
    );
}

#[test]
fn a_tile_has_one_note_that_goes_when_it_is_answered_or_the_app_returns() {
    let (mut attention, memory) = attention();
    let a = route(1);
    let id = a.session.to_string();
    attention.set_active(false);
    attention.look(&Look { asking: vec![asking(a, "Run make")], unread: 1 });
    attention.look(&Look::default());
    assert_eq!(memory.withdrawn(), vec![id.clone()], "answered elsewhere, its note goes");

    attention.look(&Look { asking: vec![asking(a, "Run make")], unread: 1 });
    let done = Finished { command: "make".into(), exit: Some(0), elapsed: Duration::from_secs(9) };
    attention.command_finished(a, "api".into(), &done, Duration::from_secs(5));
    let ids: Vec<String> = memory.posted().into_iter().map(|n| n.id).collect();
    assert_eq!(
        ids,
        vec![id.clone(), id.clone(), id.clone()],
        "each replaces the tile's last by id"
    );
    memory.clear();
    attention.look(&Look::default());
    assert!(memory.withdrawn().is_empty(), "the answer does not take back the command's note");

    attention.set_active(true);
    assert_eq!(memory.withdrawn(), vec![id], "back in front, the inbox has it all");
}

#[test]
fn a_command_notifies_when_it_ran_long_and_ended_with_the_app_away() {
    let (mut attention, memory) = attention();
    let a = route(1);
    let slow = Duration::from_secs(5);
    let long =
        Finished { command: "cargo build".into(), exit: Some(0), elapsed: Duration::from_secs(12) };
    let short = Finished { elapsed: Duration::from_secs(2), ..long.clone() };
    attention.command_finished(a, "api".into(), &long, slow);
    assert!(memory.posted().is_empty(), "in front, nothing");

    attention.set_active(false);
    attention.command_finished(a, "api".into(), &short, slow);
    assert!(memory.posted().is_empty(), "a short command ended before anyone looked away");
    attention.command_finished(a, "api".into(), &long, slow);
    let posted = memory.posted();
    assert_eq!(posted.len(), 1, "the long one: {posted:?}");
    assert_eq!(posted[0].title, "api", "titled by the tile");
    assert_eq!(
        posted[0].body,
        format!("cargo build \u{b7} {}", long.label()),
        "the command and how it ended"
    );
}

#[test]
fn the_badge_is_the_inboxs_unread_count() {
    let (mut attention, memory) = attention();
    assert_eq!(memory.badge(), None, "nothing set before the first look");
    attention.look(&Look { asking: Vec::new(), unread: 3 });
    assert_eq!(memory.badge(), Some(3), "the unread count");
    attention.set_active(false);
    attention.look(&Look::default());
    assert_eq!(memory.badge(), Some(0), "cleared with the inbox");
}

/// A focused workspace, drawn once.
fn workspace(cx: &mut TestAppContext) -> (Entity<WorkspaceView>, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut view = WorkspaceView::new(Theme::default(), None, cx);
        view.set_animation(false);
        window.focus(&view.focus, cx);
        view
    });
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();
    (view, cx)
}

/// A worker `name` on `key`, linked, with a shell for each of `sessions` in a tile; the
/// receiver keeps the link open.
fn worker(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    key: WorkerKey,
    name: &str,
    sessions: &[SessionId],
) -> (Vec<TileRef>, mpsc::Receiver<ClientMsg>) {
    let (tx, rx) = mpsc::channel(256);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    let hello = HelloAck {
        worker: slopty_core::WorkerId::new(),
        name: name.to_owned(),
        home: String::new(),
        caps: WorkerCaps::bare(Os::MacOs),
        load: 0.0,
        sessions: Vec::new(),
    };
    let me = ClientId::new();
    let tiles = view.update_in(cx, |v, _window, cx| {
        v.add_worker(key, name.to_owned(), cx);
        v.connect_worker(
            key,
            WorkerLink { me, out: tx, open_screen: factory, remote: None },
            hello,
            cx,
        );
        v.apply_sync(key, ItemSync::Snapshot { version: 0, items: Vec::new() }, cx);
        let mut tiles = Vec::new();
        for (version, session) in (1..).zip(sessions) {
            let item = Item {
                id: ItemId::new(),
                kind: ItemKind::Terminal { session: *session },
                sleeping: false,
                name: None,
            };
            tiles.push(TileRef { worker: key, item: item.id });
            v.session_opened(key, summary(*session), cx);
            v.apply_sync(key, ItemSync::Delta { version, by: me, op: ItemOp::Add(item) }, cx);
        }
        tiles
    });
    cx.run_until_parked();
    (tiles, rx)
}

fn summary(id: SessionId) -> SessionSummary {
    SessionSummary {
        id,
        title: "shell".into(),
        cwd: None,
        repo: None,
        branch: None,
        changes: None,
        started_ms: WallMs::ZERO,
        cols: 80,
        rows: 24,
        state: SessionState::Running,
        viewers: 1,
        command: Vec::new(),
        agent: None,
        progress: None,
        restored: None,
    }
}

#[gpui::test]
fn the_look_names_the_tile_and_says_what_the_agent_asks(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let session = SessionId::new();
    let key = WorkerKey::new(7);
    let (tiles, _link) = worker(&view, cx, key, "mini", &[session]);
    view.update_in(cx, |v, _window, cx| {
        v.agent_event(
            AgentEvent {
                session,
                kind: AgentKind::ClaudeCode,
                status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
                agent_session: None,
                detail: Some("$ cargo test".into()),
                attention: true,
                source: AgentSource::Hook,
                since_ms: WallMs::ZERO,
            },
            cx,
        );
    });
    cx.run_until_parked();
    view.update(cx, |v, cx| {
        let look = v.attention_look(cx);
        let title = v.item(tiles[0]).map(|i| v.tile_title(i, cx));
        assert_eq!(look.unread, v.inbox_count(), "the badge is the inbox's count");
        assert_eq!(look.unread, 1, "one agent waits");
        let [asks] = look.asking.as_slice() else { panic!("one agent asks: {look:?}") };
        assert_eq!(
            asks.route,
            Route { worker: key, item: Some(tiles[0].item), session },
            "its tile"
        );
        assert_eq!(Some(asks.title.clone()), title, "titled as its tile's header");
        assert_eq!(asks.body, "Run cargo test", "the ask line");
    });
}

#[gpui::test]
fn a_tapped_note_focuses_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (first, second) = (SessionId::new(), SessionId::new());
    let (tiles, _link) = worker(&view, cx, WorkerKey::new(7), "mini", &[first, second]);
    view.update_in(cx, |v, _window, cx| v.focus_tile(tiles[1], cx));
    cx.run_until_parked();
    let tap = view.update(cx, |v, cx| {
        assert_eq!(v.focused(), Some(tiles[1]), "on the second tile");
        let (route, _title) = v.attention_route(first, cx).expect("the first shell has a tile");
        Tap { id: first.to_string(), info: route.info(), action: None }
    });
    view.update_in(cx, |v, _window, cx| {
        v.open_notification(&tap, cx);
        assert_eq!(v.focused(), Some(tiles[0]), "the tap went to its tile");
        assert_eq!(v.pending_focus, Some(first), "and gives its shell the keyboard");
    });
}

/// An agent's note gets the approval buttons once a prompt is held for it, silently, as the
/// prompt comes a moment after the status; they go again, silently, once it is not held. A
/// prompt this client answered takes the note away instead, and it stays away while the agent
/// still reads as waiting.
#[test]
fn an_approval_note_carries_the_buttons_while_its_prompt_is_held() {
    let (mut attention, memory) = attention();
    let a = route(1);
    attention.set_active(false);
    attention.look(&Look { asking: vec![asking(a, "Run make")], unread: 1 });
    let held = Asking { approval: Some(3), ..asking(a, "Run make") };
    attention.look(&Look { asking: vec![held.clone()], unread: 1 });
    attention.look(&Look { asking: vec![held], unread: 1 });
    attention.look(&Look { asking: vec![asking(a, "Run make")], unread: 1 });
    let said: Vec<(bool, bool, Option<String>)> = memory
        .posted()
        .into_iter()
        .map(|n| (n.category == Some(APPROVAL), n.silent, n.info.get(ASK).cloned()))
        .collect();
    assert_eq!(
        said,
        [(false, false, None), (true, true, Some("3".to_owned())), (false, true, None)],
        "the first sounds; the buttons come and go without a sound"
    );
    let ids: Vec<String> = memory.posted().into_iter().map(|n| n.id).collect();
    assert!(ids.iter().all(|id| *id == a.session.to_string()), "the same note, replaced");
    assert!(memory.withdrawn().is_empty(), "{:?}", memory.withdrawn());

    memory.clear();
    let b = route(2);
    let asks = || Look { asking: vec![asking(b, "Run make")], unread: 1 };
    attention.look(&asks());
    memory.clear();
    attention.look(&Look {
        asking: vec![Asking { approval: Some(4), ..asking(b, "Run make") }],
        unread: 1,
    });
    let answered =
        Look { asking: vec![Asking { answered: Some(4), ..asking(b, "Run make") }], unread: 1 };
    attention.look(&answered);
    attention.look(&answered);
    attention.look(&asks());
    assert_eq!(memory.posted().len(), 1, "its buttons, then nothing");
    assert_eq!(memory.withdrawn(), [b.session.to_string()], "answered here, the note goes");
}

fn press(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(at.center(), gpui::Modifiers::none());
    cx.run_until_parked();
}

fn conversation(link: &mut mpsc::Receiver<ClientMsg>) -> Vec<ConversationRequest> {
    std::iter::from_fn(|| link.try_recv().ok())
        .filter_map(|msg| match msg {
            ClientMsg::Conversation(req) => Some(req),
            _ => None,
        })
        .collect()
}

fn asked(session: SessionId, ask: u64, tool: &str) -> PermissionEvent {
    PermissionEvent::Asked(Box::new(PermissionPrompt {
        session,
        ask,
        tool: tool.to_owned(),
        detail: ToolDetail::Other {
            input: Clipped { text: String::new(), lines: 0, chars: 0, full: None },
        },
        suggestions: Vec::new(),
        mode: None,
        asked_ms: WallMs::ZERO,
        until_ms: WallMs::ZERO,
    }))
}

/// The workspace asks its worker for approvals as it links. A yes or no held for it rides the
/// agent's note: "Allow" on the note answers it once and moves nothing, a second press says it
/// no longer waits, and the inbox's "Deny" answers the next without going to the agent. A
/// question never gets the buttons.
#[gpui::test]
fn an_approval_is_answered_from_the_note_and_the_inbox_where_they_are(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (session, other) = (SessionId::new(), SessionId::new());
    let key = WorkerKey::new(7);
    let (tiles, mut link) = worker(&view, cx, key, "mini", &[session, other]);
    view.update_in(cx, |v, _window, cx| {
        v.focus_tile(tiles[1], cx);
        v.agent_event(
            AgentEvent {
                session,
                kind: AgentKind::ClaudeCode,
                status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
                agent_session: None,
                detail: Some("$ cargo test".into()),
                attention: true,
                source: AgentSource::Hook,
                since_ms: WallMs::ZERO,
            },
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(conversation(&mut link), [ConversationRequest::Approvals { on: true }]);

    view.update_in(cx, |v, _window, cx| {
        v.permission_event(asked(session, 7, "AskUserQuestion"), cx);
    });
    let none = view.update(cx, |v, cx| v.attention_look(cx).asking[0].approval);
    assert_eq!(none, None, "a question waits for the conversation");

    view.update_in(cx, |v, _window, cx| v.permission_event(asked(session, 8, "Bash"), cx));
    let tap = view.update(cx, |v, cx| {
        let look = v.attention_look(cx);
        let [asks] = look.asking.as_slice() else { panic!("one agent asks: {look:?}") };
        assert_eq!(asks.approval, Some(8), "the note answers the prompt held");
        let note = asks.note(false);
        Tap { id: note.id, info: note.info, action: Some(notify::ALLOW.to_owned()) }
    });
    view.update_in(cx, |v, _window, cx| v.open_notification(&tap, cx));
    let answer = ConversationRequest::Answer { session, ask: 8, verdict: Verdict::Allow };
    assert_eq!(conversation(&mut link), [answer], "allowed once");
    view.update_in(cx, |v, _window, cx| {
        assert_eq!(v.focused(), Some(tiles[1]), "the answer moved nothing");
        v.open_notification(&tap, cx);
    });
    assert!(conversation(&mut link).is_empty(), "a second press sends nothing");

    view.update_in(cx, |v, _window, cx| v.permission_event(asked(session, 9, "Bash"), cx));
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    press(cx, "bell");
    press(cx, leak(format!("inbox-deny-{session}")));
    let deny = Verdict::Deny { message: String::new(), interrupt: false };
    let answer = ConversationRequest::Answer { session, ask: 9, verdict: deny };
    assert_eq!(conversation(&mut link), [answer], "denied from the inbox");
    view.update(cx, |v, _cx| {
        assert_eq!(v.focused(), Some(tiles[1]), "without going to the agent");
        assert!(v.approval(session).is_none(), "answered");
    });
}

/// The person looking at the session's terminal with the app in front gets Claude Code's own
/// dialog at once: the prompt is handed back rather than held for a button. With the app away,
/// it waits for the note's buttons.
#[gpui::test]
fn a_prompt_whose_terminal_is_in_front_goes_back_to_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let session = SessionId::new();
    let (tiles, mut link) = worker(&view, cx, WorkerKey::new(7), "mini", &[session]);
    view.update_in(cx, |v, _window, cx| {
        v.focus_tile(tiles[0], cx);
        v.set_app_active(false, cx);
        v.permission_event(asked(session, 3, "Bash"), cx);
    });
    cx.run_until_parked();
    assert_eq!(conversation(&mut link), [ConversationRequest::Approvals { on: true }]);
    view.update(cx, |v, _cx| assert!(v.approval(session).is_some(), "away, it waits"));
    view.update_in(cx, |v, _window, cx| v.set_app_active(true, cx));
    cx.run_until_parked();
    assert_eq!(conversation(&mut link), [ConversationRequest::Release { session, ask: 3 }]);
    view.update_in(cx, |v, _window, cx| v.permission_event(asked(session, 4, "Bash"), cx));
    cx.run_until_parked();
    let sent = conversation(&mut link);
    assert!(
        sent.contains(&ConversationRequest::Release { session, ask: 4 }),
        "handed back: {sent:?}"
    );
    view.update(cx, |v, _cx| assert!(v.approval(session).is_none(), "and not offered here"));
}

/// A note's "Allow" that comes before its prompt (the tap launched the app, or the link is
/// new and the worker has not sent what it holds) waits for it and answers it once it is here.
/// One whose prompt never comes says so once the worker has had time to send it: a toast in
/// front, the app's own note while it is away. One whose worker is not reached gives up after
/// a while.
#[gpui::test]
fn a_notes_answer_waits_for_its_prompt(cx: &mut TestAppContext) {
    use crate::workspace::inbox::approvals::{HOLD_VERDICT, NOT_REACHED, SYNCED};
    let (view, cx) = workspace(cx);
    let session = SessionId::new();
    let key = WorkerKey::new(7);
    let (tiles, mut link) = worker(&view, cx, key, "mini", &[session]);
    assert_eq!(conversation(&mut link), [ConversationRequest::Approvals { on: true }]);
    let route = Route { worker: key, item: Some(tiles[0].item), session };
    let tap = |route: Route, ask: u64, action: &str| {
        let mut info = route.info();
        info.insert(ASK.to_owned(), ask.to_string());
        Tap { id: route.session.to_string(), info, action: Some(action.to_owned()) }
    };
    let events = Rc::new(std::cell::RefCell::new(Vec::new()));
    let heard = Rc::clone(&events);
    cx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &WorkspaceEvent, _cx| {
            heard.borrow_mut().push(*event);
        })
        .detach();
    });
    let toast = |cx: &mut VisualTestContext| view.read_with(cx, WorkspaceView::toast_text);

    view.update_in(cx, |v, _window, cx| v.open_notification(&tap(route, 8, notify::ALLOW), cx));
    cx.run_until_parked();
    assert!(conversation(&mut link).is_empty(), "the prompt is not here yet: it waits");
    assert_eq!(toast(cx), None, "nothing said yet");
    view.update_in(cx, |v, _window, cx| v.permission_event(asked(session, 8, "Bash"), cx));
    cx.run_until_parked();
    let answer = ConversationRequest::Answer { session, ask: 8, verdict: Verdict::Allow };
    assert_eq!(conversation(&mut link), [answer], "answered as it came");

    view.update_in(cx, |v, _window, cx| v.open_notification(&tap(route, 9, notify::DENY), cx));
    cx.run_until_parked();
    assert_eq!(toast(cx), None, "still in time for the worker");
    cx.executor().advance_clock(SYNCED);
    cx.run_until_parked();
    assert_eq!(toast(cx).as_deref(), Some(NO_LONGER_WAITING), "the worker sent all it holds");
    assert!(conversation(&mut link).is_empty());

    view.update_in(cx, |v, _window, cx| {
        v.set_app_active(false, cx);
        v.open_notification(&tap(route, 10, notify::ALLOW), cx);
    });
    cx.run_until_parked();
    assert!(
        events.borrow().contains(&WorkspaceEvent::Unanswered { route, why: NO_LONGER_WAITING }),
        "away, the app says it in a note: {:?}",
        events.borrow()
    );

    let far = Route { worker: WorkerKey::new(99), item: None, session: SessionId::new() };
    events.borrow_mut().clear();
    view.update_in(cx, |v, _window, cx| v.open_notification(&tap(far, 1, notify::ALLOW), cx));
    cx.executor().advance_clock(SYNCED);
    cx.run_until_parked();
    assert!(events.borrow().is_empty(), "a worker not linked yet is waited for");
    cx.executor().advance_clock(HOLD_VERDICT);
    cx.run_until_parked();
    assert_eq!(
        events.borrow().as_slice(),
        [WorkspaceEvent::Unanswered { route: far, why: NOT_REACHED }],
        "until it is given up on"
    );
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

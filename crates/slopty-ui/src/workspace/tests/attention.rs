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
    Route {
        worker: WorkerKey::new(seed),
        item: Some(ItemId::new()),
        about: About::Session(SessionId::new()),
    }
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
    attention.look(&Look {
        asking: vec![asking(a, "Run cargo test")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    });
    assert!(memory.posted().is_empty(), "in front, the navigator says it");

    attention.set_active(false);
    attention.look(&Look {
        asking: vec![asking(a, "Run cargo test")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    });
    assert!(
        memory.posted().is_empty(),
        "an agent seen waiting before the app left says nothing new"
    );

    attention.look(&Look {
        asking: vec![asking(a, "Run cargo test"), asking(b, "Asks: which one?")],
        turns: Vec::new(),
        unread: 2,
        projects: HashMap::new(),
    });
    let posted = memory.posted();
    assert_eq!(posted.len(), 1, "only the agent that started to wait: {posted:?}");
    let note = &posted[0];
    assert_eq!(note.id, b.about.note_id(), "one note per tile, named by its session");
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

/// With the person at another of their devices nothing notifies here, and what was up is
/// taken back as they arrive there; once they leave it, a new moment notifies again.
#[test]
fn nothing_notifies_while_the_person_is_at_another_device() {
    let (mut attention, memory) = attention();
    let (a, b) = (route(1), route(2));
    attention.set_active(false);
    attention.look(&Look {
        asking: vec![asking(a, "Run cargo test")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    });
    assert_eq!(memory.posted().len(), 1, "away from every device: the phone says it");

    attention.set_present_elsewhere(true);
    assert_eq!(memory.withdrawn(), [a.about.note_id()], "at the Mac, the phone's note goes");
    let both = vec![asking(a, "Run cargo test"), asking(b, "Asks: which one?")];
    attention.look(&Look {
        asking: both.clone(),
        turns: Vec::new(),
        unread: 2,
        projects: HashMap::new(),
    });
    let done =
        Finished { command: "cargo build".into(), exit: Some(0), elapsed: Duration::from_secs(60) };
    attention.command_finished(route(3), "build".into(), &done, Duration::from_secs(5));
    assert_eq!(memory.posted().len(), 1, "nothing new while the Mac is in front of them");

    attention.set_present_elsewhere(false);
    let c = route(4);
    let mut three = both;
    three.push(asking(c, "Asks: ship it?"));
    attention.look(&Look { asking: three, turns: Vec::new(), unread: 3, projects: HashMap::new() });
    let posted = memory.posted();
    assert_eq!(posted.len(), 2, "gone from the Mac, a new wait notifies: {posted:?}");
    assert_eq!(posted[1].id, c.about.note_id());
}

/// Led by the server, the look posts no agent moment of its own: only what the server sends
/// does, and only while the app is away. The look still gives a wait's note its approval
/// buttons and takes it back once answered; a shell's command still notifies here.
#[test]
fn led_by_the_server_only_its_notices_post_for_agents() {
    let (mut attention, memory) = attention();
    let a = route(1);
    attention.set_server_led(true);
    attention.set_active(false);
    let turn = Turn { route: route(2), title: "api".into(), body: "Done".into() };
    attention.look(&Look {
        asking: vec![asking(a, "Run cargo test")],
        turns: vec![turn],
        unread: 2,
        projects: HashMap::new(),
    });
    assert!(memory.posted().is_empty(), "the look decides nothing: {:?}", memory.posted());

    let heard = Heard {
        route: a,
        kind: NoticeKind::NeedsYou,
        title: "Fix the build".into(),
        body: "Run cargo test".into(),
    };
    attention.notice(&heard);
    let posted = memory.posted();
    assert_eq!(posted.len(), 1);
    assert_eq!((posted[0].title.as_str(), posted[0].category), ("Fix the build", None));

    let held = Asking { approval: Some("7".into()), ..asking(a, "Run cargo test") };
    attention.look(&Look {
        asking: vec![held],
        turns: Vec::new(),
        unread: 2,
        projects: HashMap::new(),
    });
    let posted = memory.posted();
    assert_eq!(posted.len(), 2, "the note again, now with the buttons");
    assert_eq!(posted[1].category, Some(APPROVAL));
    assert!(posted[1].silent, "replaced without a second sound");

    attention.look(&Look::default());
    assert_eq!(memory.withdrawn(), [a.about.note_id()], "answered, its note goes");

    let done =
        Finished { command: "cargo build".into(), exit: Some(0), elapsed: Duration::from_secs(60) };
    attention.command_finished(route(3), "build".into(), &done, Duration::from_secs(5));
    assert_eq!(memory.posted().len(), 3, "a shell's moment is this client's own");

    attention.set_active(true);
    attention.notice(&heard);
    assert_eq!(memory.posted().len(), 3, "in front, the navigator says it");
}

#[test]
fn a_tile_has_one_note_that_goes_when_it_is_answered_or_the_app_returns() {
    let (mut attention, memory) = attention();
    let a = route(1);
    let id = a.about.note_id();
    attention.set_active(false);
    attention.look(&Look {
        asking: vec![asking(a, "Run make")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    });
    attention.look(&Look::default());
    assert_eq!(memory.withdrawn(), vec![id.clone()], "answered elsewhere, its note goes");

    attention.look(&Look {
        asking: vec![asking(a, "Run make")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    });
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
    assert_eq!(memory.withdrawn(), vec![id], "back in front, the navigator has it all");
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
fn the_badge_is_the_bells_count() {
    let (mut attention, memory) = attention();
    assert_eq!(memory.badge(), None, "nothing set before the first look");
    attention.look(&Look {
        asking: Vec::new(),
        turns: Vec::new(),
        unread: 3,
        projects: HashMap::new(),
    });
    assert_eq!(memory.badge(), Some(3), "the bell's count");
    attention.set_active(false);
    attention.look(&Look::default());
    assert_eq!(memory.badge(), Some(0), "cleared with the bell");
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
        settings: String::new(),
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
                name: None,
                facts: BTreeMap::new(),
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
        repo_id: None,
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
                mode: None,
            },
            cx,
        );
    });
    cx.run_until_parked();
    view.update(cx, |v, _| {
        let look = v.attention_look();
        let title = v.item(tiles[0]).map(|i| v.tile_title(i));
        assert_eq!(look.unread, v.bell_count(), "the badge is the bell's count");
        assert_eq!(look.unread, 1, "one agent waits");
        let [asks] = look.asking.as_slice() else { panic!("one agent asks: {look:?}") };
        assert_eq!(
            asks.route,
            Route { worker: key, item: Some(tiles[0].item), about: About::Session(session) },
            "its tile"
        );
        assert_eq!(Some(asks.title.clone()), title, "titled as its tile's header");
        assert_eq!(asks.body, "Run cargo test", "the ask line");
    });
}

/// The server's notice becomes a note that leads to the thread's tile, says the subagent it
/// came from, and is dropped for a turn shorter than the slow-command time.
#[gpui::test]
fn a_server_notice_leads_to_its_tile_and_names_the_subagent(cx: &mut TestAppContext) {
    use slopty_proto::thread::ThreadId;
    use slopty_proto::thread::attention::{ThreadAt, Via};

    let (view, cx) = workspace(cx);
    let session = SessionId::new();
    let id = slopty_core::WorkerId::new();
    let key = crate::workspace::projects::worker_key(id);
    let (tiles, _link) = worker(&view, cx, key, "mini", &[session]);
    let mut notice = Notice {
        kind: NoticeKind::NeedsYou,
        thread: ThreadAt { worker: id, thread: ThreadId::new() },
        tile: Some(session),
        title: "Fix the build".into(),
        text: "Run cargo test".into(),
        worked_ms: None,
        via: Some(Via { thread: ThreadId::new(), title: "Explore the tests".into() }),
    };
    view.update(cx, |v, _| {
        let heard = v.heard(&notice).expect("a note");
        assert_eq!(
            heard.route,
            Route { worker: key, item: Some(tiles[0].item), about: About::Session(session) }
        );
        assert_eq!(heard.title, "Fix the build");
        assert_eq!(heard.body, "Explore the tests: Run cargo test", "the subagent named");
    });
    notice.kind = NoticeKind::Finished;
    notice.worked_ms = Some(1_000);
    assert!(view.read_with(cx, |v, _| v.heard(&notice)).is_none(), "a short turn says nothing");
    notice.worked_ms = Some(600_000);
    assert!(view.read_with(cx, |v, _| v.heard(&notice)).is_some(), "a long one does");

    // A thread driven over a protocol has no terminal: its note is its own, and a tap on it
    // opens the thread.
    notice.tile = None;
    let heard = view.read_with(cx, |v, _| v.heard(&notice)).expect("a thread's note");
    let thread = notice.thread.thread;
    assert_eq!(heard.route, Route { worker: key, item: None, about: About::Thread(thread) });
    let (mut attention, memory) = attention();
    attention.set_active(false);
    attention.notice(&heard);
    let posted = memory.posted();
    let [note] = posted.as_slice() else { panic!("one note: {posted:?}") };
    assert_eq!(note.id, About::Thread(thread).note_id(), "not a session's");
    let tap = Tap { id: note.id.clone(), info: note.info.clone(), action: None };
    assert_eq!(Route::of_tap(&tap), Some(heard.route), "the tap leads back to it");
}

#[gpui::test]
fn a_tapped_note_focuses_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (first, second) = (SessionId::new(), SessionId::new());
    let (tiles, _link) = worker(&view, cx, WorkerKey::new(7), "mini", &[first, second]);
    view.update_in(cx, |v, _window, cx| v.focus_tile(tiles[1], cx));
    cx.run_until_parked();
    let tap = view.update(cx, |v, _| {
        assert_eq!(v.focused(), Some(tiles[1]), "on the second tile");
        let (route, _title) = v.attention_route(first).expect("the first shell has a tile");
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
    attention.look(&Look {
        asking: vec![asking(a, "Run make")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    });
    let held = Asking { approval: Some("3".into()), ..asking(a, "Run make") };
    attention.look(&Look {
        asking: vec![held.clone()],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    });
    attention.look(&Look {
        asking: vec![held],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    });
    attention.look(&Look {
        asking: vec![asking(a, "Run make")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    });
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
    assert!(ids.iter().all(|id| *id == a.about.note_id()), "the same note, replaced");
    assert!(memory.withdrawn().is_empty(), "{:?}", memory.withdrawn());

    memory.clear();
    let b = route(2);
    let asks = || Look {
        asking: vec![asking(b, "Run make")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    };
    attention.look(&asks());
    memory.clear();
    attention.look(&Look {
        asking: vec![Asking { approval: Some("4".into()), ..asking(b, "Run make") }],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    });
    let answered = Look {
        asking: vec![Asking { answered: Some("4".into()), ..asking(b, "Run make") }],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
    };
    attention.look(&answered);
    attention.look(&answered);
    attention.look(&asks());
    assert_eq!(memory.posted().len(), 1, "its buttons, then nothing");
    assert_eq!(memory.withdrawn(), [b.about.note_id()], "answered here, the note goes");
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
/// no longer waits, and its row's "Deny" and "Allow" under *Needs you* answer the next ones
/// without going to the agent. A question never gets the buttons.
#[gpui::test]
fn an_approval_is_answered_from_the_note_and_its_row_where_they_are(cx: &mut TestAppContext) {
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
                mode: None,
            },
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(conversation(&mut link), [ConversationRequest::Approvals { on: true }]);

    view.update_in(cx, |v, _window, cx| {
        v.permission_event(asked(session, 7, "AskUserQuestion"), cx);
    });
    let none = view.update(cx, |v, _| v.attention_look().asking[0].approval.clone());
    assert_eq!(none, None, "a question waits for the conversation");

    view.update_in(cx, |v, _window, cx| v.permission_event(asked(session, 8, "Bash"), cx));
    let tap = view.update(cx, |v, _| {
        let look = v.attention_look();
        let [asks] = look.asking.as_slice() else { panic!("one agent asks: {look:?}") };
        assert_eq!(asks.approval.as_deref(), Some("8"), "the note answers the prompt held");
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
    cx.run_until_parked();
    press(cx, leak(format!("nav-deny-{session}")));
    let deny = Verdict::Deny { message: String::new(), interrupt: false };
    let answer = ConversationRequest::Answer { session, ask: 9, verdict: deny };
    assert_eq!(conversation(&mut link), [answer], "denied from its row");
    view.update(cx, |v, _cx| {
        assert_eq!(v.focused(), Some(tiles[1]), "without going to the agent");
        assert!(v.approval(session).is_none(), "answered");
    });

    view.update_in(cx, |v, _window, cx| v.permission_event(asked(session, 10, "Bash"), cx));
    cx.run_until_parked();
    press(cx, leak(format!("nav-allow-{session}")));
    let answer = ConversationRequest::Answer { session, ask: 10, verdict: Verdict::Allow };
    assert_eq!(conversation(&mut link), [answer], "allowed from its row");
    assert!(
        cx.debug_bounds(leak(format!("nav-allow-{session}"))).is_none(),
        "answered, the row has no buttons"
    );
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
    use crate::workspace::approvals::{HOLD_VERDICT, NOT_REACHED, SYNCED};
    let (view, cx) = workspace(cx);
    let session = SessionId::new();
    let key = WorkerKey::new(7);
    let (tiles, mut link) = worker(&view, cx, key, "mini", &[session]);
    assert_eq!(conversation(&mut link), [ConversationRequest::Approvals { on: true }]);
    let route = Route { worker: key, item: Some(tiles[0].item), about: About::Session(session) };
    let tap = |route: Route, ask: u64, action: &str| {
        let mut info = route.info();
        info.insert(ASK.to_owned(), ask.to_string());
        Tap { id: route.about.note_id(), info, action: Some(action.to_owned()) }
    };
    let events = Rc::new(std::cell::RefCell::new(Vec::new()));
    let heard = Rc::clone(&events);
    cx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &WorkspaceEvent, _cx| {
            heard.borrow_mut().push(*event);
        })
        .detach();
    });
    let toast = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.toast_text());

    view.update_in(cx, |v, _window, cx| v.open_notification(&tap(route, 8, notify::ALLOW), cx));
    cx.run_until_parked();
    assert!(conversation(&mut link).is_empty(), "the prompt is not here yet: it waits");
    assert_eq!(toast(cx), None, "nothing said yet");
    view.update_in(cx, |v, _window, cx| v.permission_event(asked(session, 8, "Bash"), cx));
    cx.run_until_parked();
    let answer = ConversationRequest::Answer { session, ask: 8, verdict: Verdict::Allow };
    assert_eq!(conversation(&mut link), [answer], "answered as it came");
    assert_eq!(
        events.borrow().as_slice(),
        [WorkspaceEvent::TapsSettled],
        "out: an app woken for it may sleep again"
    );
    events.borrow_mut().clear();

    view.update_in(cx, |v, _window, cx| v.open_notification(&tap(route, 9, notify::DENY), cx));
    cx.run_until_parked();
    assert_eq!(toast(cx), None, "still in time for the worker");
    cx.executor().advance_clock(SYNCED);
    cx.run_until_parked();
    assert_eq!(toast(cx).as_deref(), Some(NO_LONGER_WAITING), "the worker sent all it holds");
    assert_eq!(conversation(&mut link), Vec::<ConversationRequest>::new());

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

    let far =
        Route { worker: WorkerKey::new(99), item: None, about: About::Session(SessionId::new()) };
    events.borrow_mut().clear();
    view.update_in(cx, |v, _window, cx| v.open_notification(&tap(far, 1, notify::ALLOW), cx));
    cx.executor().advance_clock(SYNCED);
    cx.run_until_parked();
    assert!(events.borrow().is_empty(), "a worker not linked yet is waited for");
    cx.executor().advance_clock(HOLD_VERDICT);
    cx.run_until_parked();
    assert_eq!(
        events.borrow().as_slice(),
        [WorkspaceEvent::Unanswered { route: far, why: NOT_REACHED }, WorkspaceEvent::TapsSettled],
        "until it is given up on"
    );
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// An agent's turn that finished unwatched notifies once, while the app is away, with what it
/// said and how long it ran; in front, the navigator has it.
#[test]
fn a_finished_turn_notifies_once_while_the_app_is_away() {
    let (mut attention, memory) = attention();
    let a = route(1);
    let turn = Turn { route: a, title: "api".into(), body: "Fixed the test · Done · 2m".into() };
    let look =
        Look { asking: Vec::new(), turns: vec![turn.clone()], unread: 1, projects: HashMap::new() };
    attention.look(&look);
    assert!(memory.posted().is_empty(), "in front, the navigator says it");

    attention.look(&Look::default());
    attention.set_active(false);
    attention.look(&look);
    attention.look(&look);
    let posted = memory.posted();
    assert_eq!(posted.len(), 1, "once: {posted:?}");
    assert_eq!((posted[0].title.as_str(), posted[0].body.as_str()), ("api", turn.body.as_str()));
    assert_eq!(posted[0].id, a.about.note_id(), "the tile's one note");
}

fn agent(
    session: SessionId,
    status: AgentStatus,
    since_s: u64,
    detail: Option<&str>,
) -> AgentEvent {
    AgentEvent {
        session,
        kind: AgentKind::ClaudeCode,
        status,
        agent_session: None,
        detail: detail.map(str::to_owned),
        attention: false,
        source: AgentSource::Hook,
        since_ms: WallMs::from_millis(since_s.saturating_mul(1000)),
        mode: None,
    }
}

/// A turn that ran long and finished on a tile not in focus earns a badge, a count on the bell,
/// a row under *To review* and a turn in the look; focusing the tile reads it. A turn on the
/// focused tile, a short one, and one that went idle without finishing earn nothing.
#[gpui::test]
fn an_agent_finishing_out_of_sight_is_left_to_review(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (away, here) = (SessionId::new(), SessionId::new());
    let (tiles, _link) = worker(&view, cx, WorkerKey::new(7), "mini", &[away, here]);
    view.update_in(cx, |v, _window, cx| {
        v.focus_tile(tiles[1], cx);
        for session in [away, here] {
            v.agent_event(agent(session, AgentStatus::Working, 100, None), cx);
            let tool = AgentStatus::Tool { tool: "Bash".into() };
            v.agent_event(agent(session, tool, 130, None), cx);
            v.agent_event(agent(session, AgentStatus::Done, 220, Some("Fixed the test")), cx);
        }
    });
    cx.run_until_parked();
    view.update(cx, |v, _| {
        assert_eq!(v.bell_count(), 1, "the one out of sight");
        let look = v.attention_look();
        let [turn] = look.turns.as_slice() else { panic!("one turn: {look:?}") };
        assert_eq!(turn.route.about, About::Session(away));
        assert_eq!(turn.body, "Fixed the test \u{b7} Done \u{b7} 2m 0s");
    });
    assert!(cx.debug_bounds(leak(format!("nav-review-{away}"))).is_some(), "its row");

    let short = SessionId::new();
    let idle = SessionId::new();
    view.update_in(cx, |v, _window, cx| {
        v.focus_tile(tiles[0], cx);
        v.agent_event(agent(short, AgentStatus::Working, 300, None), cx);
        v.agent_event(agent(short, AgentStatus::Done, 301, None), cx);
        v.agent_event(agent(idle, AgentStatus::Working, 300, None), cx);
        v.agent_event(agent(idle, AgentStatus::Idle, 400, None), cx);
        v.agent_event(agent(idle, AgentStatus::Done, 500, None), cx);
    });
    cx.run_until_parked();
    view.update(cx, |v, _| {
        assert_eq!(v.bell_count(), 0, "read by its focus; nothing else earned a row");
        assert_eq!(v.attention_look().turns, Vec::<Turn>::new());
    });
}

/// An agent whose turn ended unseen is listed under *To review* in the navigator, its own tile's
/// row in view or not, saying what it did; a press goes to it, which reads it, and the section
/// goes.
#[gpui::test]
fn an_agent_that_ended_unseen_is_listed_to_review(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (away, here) = (SessionId::new(), SessionId::new());
    let key = WorkerKey::new(7);
    let (tiles, _link) = worker(&view, cx, key, "mini", &[away, here]);
    view.update_in(cx, |v, _window, cx| {
        v.focus_tile(tiles[1], cx);
        v.agent_event(agent(away, AgentStatus::Working, 100, None), cx);
        v.agent_event(agent(away, AgentStatus::Done, 220, Some("Fixed the test")), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("nav-to-review").is_some(), "the section lists it");
    let words = view.read_with(cx, |v, _| v.to_review());
    assert_eq!(words.iter().map(|w| w.session).collect::<Vec<_>>(), [away]);
    press(cx, leak(format!("nav-review-{away}")));
    assert_eq!(view.read_with(cx, |v, _| v.focused()), Some(tiles[0]), "it went there");
    assert!(cx.debug_bounds("nav-to-review").is_none(), "looked at, nothing is left to review");
}

/// The notes of one project stack in one thread, keyed by its group: two agents in atlas share
/// one, the agent in site has its own.
#[gpui::test]
fn notes_of_one_project_share_a_thread(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let key = WorkerKey::new(7);
    let sessions = [SessionId::new(), SessionId::new(), SessionId::new()];
    let (tiles, _link) = worker(&view, cx, key, "mini", &sessions);
    view.update_in(cx, |v, _window, cx| {
        for (session, repo) in sessions.iter().zip(["/w/atlas", "/w/atlas", "/w/site"]) {
            let placed = SessionSummary {
                cwd: Some(repo.to_owned()),
                repo: Some(repo.to_owned()),
                ..summary(*session)
            };
            v.session_opened(key, placed, cx);
            let waits = AgentEvent {
                session: *session,
                kind: AgentKind::ClaudeCode,
                status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
                agent_session: None,
                detail: Some("$ cargo test".into()),
                attention: true,
                source: AgentSource::Hook,
                since_ms: WallMs::ZERO,
                mode: None,
            };
            v.agent_event(waits, cx);
        }
    });
    cx.run_until_parked();
    let (look, atlas, site) = view.read_with(cx, |v, _| {
        let groups = v.project_groups();
        let key = |t| groups.group_of(t).map(|g| g.key.as_str().to_owned()).expect("a group");
        (v.attention_look(), key(tiles[0]), key(tiles[2]))
    });
    assert_ne!(atlas, site);
    let (mut attention, memory) = attention();
    attention.set_active(false);
    attention.look(&look);
    let thread_of = |session: SessionId| {
        memory.posted().into_iter().find(|n| n.id == session.to_string()).and_then(|n| n.thread)
    };
    assert_eq!(thread_of(sessions[0]), Some(atlas.clone()), "atlas's thread");
    assert_eq!(thread_of(sessions[1]), Some(atlas), "the same one");
    assert_eq!(thread_of(sessions[2]), Some(site), "site's own");
}

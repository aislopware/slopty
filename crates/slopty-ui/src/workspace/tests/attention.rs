//! What notifies on a pocketed phone, against a notifier that shows nothing: the decisions on
//! their own, then a headless workspace's look and a tapped note's way back to its tile.

use std::sync::Arc;

use gpui::{Entity, TestAppContext, VisualTestContext, px, size};
use slopty_agent::status::{AgentEvent, AgentSource, AgentStatus, BlockReason};
use slopty_core::{ClientId, WallMs};
use slopty_platform::notify::Memory;
use slopty_proto::ClientMsg;
use slopty_proto::handshake::HelloAck;
use slopty_proto::items::{Item, ItemOp, ItemSync};
use slopty_proto::server::{Os, WorkerCaps};
use slopty_proto::terminal::{SessionState, SessionSummary};
use slopty_proto::thread::wire::{Intent, RequestCard, TableFrame, ThreadRequest, ThreadRow};
use slopty_proto::thread::{AskId, Choice, Cursor, Effect, Request, ThreadState};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use super::*;
use crate::screen::ScreenFactory;
use crate::workspace::tests::Agents as _;
use crate::workspace::{WorkerLink, WorkspaceEvent};

fn route(seed: u128) -> Route {
    Route {
        worker: WorkerKey::new(seed),
        item: Some(ItemId::new()),
        about: About::Session(SessionId::new()),
    }
}

fn asking(route: Route, body: &str) -> Asking {
    Asking {
        route,
        title: "api".into(),
        body: body.into(),
        approval: None,
        answered: None,
        own: false,
    }
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
        muted: HashSet::new(),
    });
    assert!(memory.posted().is_empty(), "in front, the navigator says it");

    attention.set_active(false);
    attention.look(&Look {
        asking: vec![asking(a, "Run cargo test")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
        muted: HashSet::new(),
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
        muted: HashSet::new(),
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
        Route::of_tap(&Tap {
            id: note.id.clone(),
            info: note.info.clone(),
            action: None,
            text: None
        }),
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
        muted: HashSet::new(),
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
        muted: HashSet::new(),
    });
    let done = Finished {
        command: "cargo build".into(),
        exit: Some(0),
        elapsed: Duration::from_secs(60),
        turn: None,
    };
    attention.command_finished(route(3), "build".into(), &done, Duration::from_secs(5));
    assert_eq!(memory.posted().len(), 1, "nothing new while the Mac is in front of them");

    attention.set_present_elsewhere(false);
    let c = route(4);
    let mut three = both;
    three.push(asking(c, "Asks: ship it?"));
    attention.look(&Look {
        asking: three,
        turns: Vec::new(),
        unread: 3,
        projects: HashMap::new(),
        muted: HashSet::new(),
    });
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
        muted: HashSet::new(),
    });
    assert!(memory.posted().is_empty(), "the look decides nothing: {:?}", memory.posted());

    let heard = Heard {
        route: a,
        kind: NoticeKind::NeedsYou,
        title: "Fix the build".into(),
        body: "Run cargo test".into(),
        stack: None,
    };
    attention.notice(&heard);
    let posted = memory.posted();
    assert_eq!(posted.len(), 1);
    assert_eq!(
        (posted[0].title.as_str(), posted[0].category),
        ("Fix the build", Some(REPLYING)),
        "a reply until a yes or no is there"
    );

    let held = Asking { approval: Some("7".into()), ..asking(a, "Run cargo test") };
    attention.look(&Look {
        asking: vec![held],
        turns: Vec::new(),
        unread: 2,
        projects: HashMap::new(),
        muted: HashSet::new(),
    });
    let posted = memory.posted();
    assert_eq!(posted.len(), 2, "the note again, now with the buttons");
    assert_eq!(posted[1].category, Some(APPROVAL));
    assert!(posted[1].silent, "replaced without a second sound");

    attention.look(&Look::default());
    assert_eq!(memory.withdrawn(), [a.about.note_id()], "answered, its note goes");

    let done = Finished {
        command: "cargo build".into(),
        exit: Some(0),
        elapsed: Duration::from_secs(60),
        turn: None,
    };
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
        muted: HashSet::new(),
    });
    attention.look(&Look::default());
    assert_eq!(memory.withdrawn(), vec![id.clone()], "answered elsewhere, its note goes");

    attention.look(&Look {
        asking: vec![asking(a, "Run make")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
        muted: HashSet::new(),
    });
    let done = Finished {
        command: "make".into(),
        exit: Some(0),
        elapsed: Duration::from_secs(9),
        turn: None,
    };
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
    let long = Finished {
        command: "cargo build".into(),
        exit: Some(0),
        elapsed: Duration::from_secs(12),
        turn: None,
    };
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

/// Only an agent that needs the person is Time Sensitive, breaking through a Focus: as the look
/// sees it, with its buttons and without, and as the server says it, a project's included. A
/// finished turn, a failure, a long command and a program's own note wait like any other.
#[test]
fn only_needs_you_breaks_through_a_focus() {
    let (mut own, memory) = attention();
    own.set_active(false);
    let (a, b, c, d) = (route(1), route(2), route(3), route(4));
    own.look(&Look {
        asking: vec![asking(a, "Run cargo test")],
        turns: vec![Turn { route: b, title: "api".into(), body: "Done".into() }],
        unread: 2,
        projects: HashMap::new(),
        muted: HashSet::new(),
    });
    let held = Asking { approval: Some("7".into()), ..asking(a, "Run cargo test") };
    own.look(&Look { asking: vec![held], ..Look::default() });
    let done = Finished {
        command: "cargo build".into(),
        exit: Some(1),
        elapsed: Duration::from_secs(60),
        turn: None,
    };
    own.command_finished(c, "build".into(), &done, Duration::from_secs(5));
    own.program(d, "vim".into(), "Saved".into());
    let urgent: Vec<(String, bool)> =
        memory.posted().into_iter().map(|n| (n.id, n.urgent)).collect();
    let id = |r: Route| r.about.note_id();
    let expected = [(id(a), true), (id(b), false), (id(a), true), (id(c), false), (id(d), false)];
    assert_eq!(urgent, expected, "asks only");

    let (mut led, memory) = attention();
    led.set_server_led(true);
    led.set_active(false);
    let stack = |id: &str| Some(Stack { id: id.into(), project: "p".into() });
    let notices = [
        (NoticeKind::NeedsYou, None),
        (NoticeKind::Failed, None),
        (NoticeKind::Finished, None),
        (NoticeKind::Project, None),
        (NoticeKind::NeedsYou, stack("s1")),
        (NoticeKind::Project, stack("s2")),
    ];
    for (kind, stack) in notices {
        let (title, body) = ("t".into(), "b".into());
        led.notice(&Heard { route: route(5), kind, title, body, stack });
    }
    let urgent: Vec<bool> = memory.posted().into_iter().map(|n| n.urgent).collect();
    assert_eq!(urgent, [true, false, false, false, true, false], "the server's, by their kind");
}

/// With notifications off, a note that goes out while the app is away reaches nobody: back in
/// front, the app says so, once a run. Nothing is said while they are on, nor when nothing went
/// unsaid, nor a second time.
#[test]
fn notes_turned_off_are_said_once_on_coming_back() {
    let (mut attention, memory) = attention();
    let a = route(1);
    let long = Finished {
        command: "cargo build".into(),
        exit: Some(0),
        elapsed: Duration::from_secs(12),
        turn: None,
    };
    let slow = Duration::from_secs(5);
    let away_and_back = |attention: &mut Attention| {
        attention.set_active(false);
        attention.command_finished(a, "api".into(), &long, slow);
        attention.set_active(true);
        attention.unsaid_while_off()
    };
    assert!(!away_and_back(&mut attention), "allowed: the note reached the person");

    memory.set_alerts(Alerts::Denied);
    attention.set_active(false);
    attention.set_active(true);
    assert!(!attention.unsaid_while_off(), "off, but nothing went unsaid");
    assert!(away_and_back(&mut attention), "off, and a note went unsaid");
    assert!(!away_and_back(&mut attention), "said once a run");
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
        muted: HashSet::new(),
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
        progress: None,
        restored: None,
        program: Vec::new(),
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
    use slopty_proto::orchestration::TermRef;
    use slopty_proto::thread::ThreadId;
    use slopty_proto::thread::attention::{Subject, ThreadAt, Via};

    let (view, cx) = workspace(cx);
    let session = SessionId::new();
    let id = slopty_core::WorkerId::new();
    let key = crate::workspace::projects::worker_key(id);
    let (tiles, _link) = worker(&view, cx, key, "mini", &[session]);
    let thread = ThreadId::new();
    let mut notice = Notice {
        kind: NoticeKind::NeedsYou,
        about: Subject::Thread(ThreadAt { worker: id, thread }),
        tile: Some(TermRef { worker: id, session }),
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
    assert_eq!(heard.route, Route { worker: key, item: None, about: About::Thread(thread) });
    let (mut attention, memory) = attention();
    attention.set_active(false);
    attention.notice(&heard);
    let posted = memory.posted();
    let [note] = posted.as_slice() else { panic!("one note: {posted:?}") };
    assert_eq!(note.id, About::Thread(thread).note_id(), "not a session's");
    let tap = Tap { id: note.id.clone(), info: note.info.clone(), action: None, text: None };
    assert_eq!(Route::of_tap(&tap), Some(heard.route), "the tap leads back to it");
}

/// A note pushed while the app is suspended is the note it would have posted for the same
/// notice: the same identifier, words, urgency and route back. It knows no tile, which a tap
/// finds by the session or the thread. Once the app has said it stops listening, it posts
/// none of the server's notices itself, so a moment is not said twice.
#[gpui::test]
fn a_pushed_note_is_the_note_the_app_would_post(cx: &mut TestAppContext) {
    use slopty_proto::orchestration::TermRef;
    use slopty_proto::push::PushBody;
    use slopty_proto::thread::ThreadId;
    use slopty_proto::thread::attention::{Subject, ThreadAt, Via};

    let (view, cx) = workspace(cx);
    let session = SessionId::new();
    let id = slopty_core::WorkerId::new();
    let key = crate::workspace::projects::worker_key(id);
    let (_tiles, _link) = worker(&view, cx, key, "mini", &[session]);
    let thread = ThreadId::new();
    let notice = |kind, tile: Option<TermRef>, via: Option<Via>| Notice {
        kind,
        about: Subject::Thread(ThreadAt { worker: id, thread }),
        tile,
        title: "Fix the build".into(),
        text: "Run cargo test".into(),
        worked_ms: Some(600_000),
        via,
    };
    let tile = Some(TermRef { worker: id, session });
    let explore = Some(Via { thread: ThreadId::new(), title: "Explore the tests".into() });
    for notice in [
        notice(NoticeKind::NeedsYou, tile, explore),
        notice(NoticeKind::Finished, tile, None),
        notice(NoticeKind::Failed, None, None),
    ] {
        let heard = view.read_with(cx, |v, _| v.heard(&notice)).expect("a note");
        let (mut attention, memory) = attention();
        attention.set_active(false);
        attention.notice(&heard);
        let posted = memory.posted();
        let [posted] = posted.as_slice() else { panic!("one note: {posted:?}") };
        let pushed =
            notify::pushed::note_of(&PushBody { notice: notice.clone(), ask: None, quiet: false });
        let mut info = posted.info.clone();
        info.remove(ITEM);
        let what = |n: &Note| (n.id.clone(), n.title.clone(), n.body.clone(), n.urgent, n.category);
        assert_eq!(what(&pushed), what(posted), "{notice:?}");
        assert_eq!(pushed.info, info, "all but the tile, {notice:?}");
        let tap = |n: &Note| {
            Route::of_tap(&Tap { id: n.id.clone(), info: n.info.clone(), action: None, text: None })
        };
        let routed = tap(&pushed).expect("a pushed note routes");
        assert_eq!((routed.worker, routed.about), (heard.route.worker, heard.route.about));

        attention.set_listening(false);
        attention.notice(&heard);
        assert_eq!(memory.posted().len(), 1, "not listening, the push says it");
    }
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
        Tap { id: first.to_string(), info: route.info(), action: None, text: None }
    });
    view.update_in(cx, |v, _window, cx| {
        v.open_notification(&tap, cx);
        assert_eq!(v.focused(), Some(tiles[0]), "the tap went to its tile");
        assert_eq!(v.pending_focus, Some(first), "and gives its shell the keyboard");
    });
}

/// A note tapped on a cold phone, while its machine is still being dialled, says so and waits:
/// once that machine has sent what it holds, the agent with no tile here gets one. A
/// tap that waited past [`PARKED_FOR`] goes nowhere.
#[gpui::test]
fn a_note_tapped_before_its_machine_links_goes_there_once_it_does(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let opened = |rx: &mut mpsc::Receiver<ClientMsg>, session: SessionId| {
        std::iter::from_fn(|| rx.try_recv().ok()).any(|msg| {
            matches!(msg, ClientMsg::Items(ItemOp::Add(Item {
                kind: ItemKind::Terminal { session: s }, ..
            })) if s == session)
        })
    };
    let tap_on = |key: WorkerKey, session: SessionId| {
        let route = Route { worker: key, item: None, about: About::Session(session) };
        Tap { id: session.to_string(), info: route.info(), action: None, text: None }
    };

    let (studio, session) = (WorkerKey::new(9), SessionId::new());
    view.update_in(cx, |v, _w, cx| {
        v.hold_clock(Some(Duration::ZERO));
        v.add_worker(studio, "studio".to_owned(), cx);
        v.open_notification(&tap_on(studio, session), cx);
    });
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("Connecting to studio…"), "the tap waits for the link");
    view.update(cx, |v, _| v.hold_clock(Some(Duration::from_secs(5))));
    let (_tiles, mut link) = worker(&view, cx, studio, "studio", &[]);
    assert!(opened(&mut link, session), "linked, the agent gets its tile");

    let (late, other) = (WorkerKey::new(10), SessionId::new());
    view.update_in(cx, |v, _w, cx| {
        v.hold_clock(Some(Duration::from_secs(10)));
        v.add_worker(late, "mini".to_owned(), cx);
        v.open_notification(&tap_on(late, other), cx);
        let past = PARKED_FOR.saturating_add(Duration::from_secs(11));
        v.hold_clock(Some(past));
    });
    let (_tiles, mut link) = worker(&view, cx, late, "mini", &[]);
    assert!(!opened(&mut link, other), "a link past the wait leaves the person where they are");
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
        muted: HashSet::new(),
    });
    let held = Asking { approval: Some("3".into()), ..asking(a, "Run make") };
    attention.look(&Look {
        asking: vec![held.clone()],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
        muted: HashSet::new(),
    });
    attention.look(&Look {
        asking: vec![held],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
        muted: HashSet::new(),
    });
    attention.look(&Look {
        asking: vec![asking(a, "Run make")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
        muted: HashSet::new(),
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
        muted: HashSet::new(),
    };
    attention.look(&asks());
    memory.clear();
    attention.look(&Look {
        asking: vec![Asking { approval: Some("4".into()), ..asking(b, "Run make") }],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
        muted: HashSet::new(),
    });
    let answered = Look {
        asking: vec![Asking { answered: Some("4".into()), ..asking(b, "Run make") }],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
        muted: HashSet::new(),
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

/// The thread requests the workspace sent on `link`.
fn thread_sent(link: &mut mpsc::Receiver<ClientMsg>) -> Vec<ThreadRequest> {
    std::iter::from_fn(|| link.try_recv().ok())
        .filter_map(|msg| match msg {
            ClientMsg::Thread(req) => Some(req),
            _ => None,
        })
        .collect()
}

/// The intents among `sent`, by the request they answer or hand back.
fn intents(sent: &[ThreadRequest]) -> Vec<Intent> {
    sent.iter()
        .filter_map(|r| match r {
            ThreadRequest::Intent {
                intent: i @ (Intent::Answer { .. } | Intent::Release { .. }),
                ..
            } => Some(i.clone()),
            _ => None,
        })
        .collect()
}

fn allowed(ask: &str) -> Intent {
    Intent::Answer { ask: AskId(ask.to_owned()), choice: "accept".to_owned(), message: None }
}

fn denied(ask: &str) -> Intent {
    Intent::Answer { ask: AskId(ask.to_owned()), choice: "decline".to_owned(), message: None }
}

/// The thread whose TUI runs in `session`, its row asking `ask` of `kind`, or nothing.
fn asking_row(state: &ThreadState, ask: Option<(&str, &str)>) -> ThreadRow {
    let choice = |id: &str, effect| Choice {
        id: id.to_owned(),
        label: id.to_owned(),
        effect,
        scope: None,
        stops: false,
    };
    let mut row = state.row(WallMs::ZERO);
    row.requests = ask
        .map(|(id, kind)| RequestCard {
            id: AskId(id.to_owned()),
            item: None,
            kind: kind.to_owned(),
            title: "Run `cargo test`".to_owned(),
            options: vec![choice("accept", Effect::Allow), choice("decline", Effect::Deny)],
            opened_ms: WallMs::ZERO,
        })
        .into_iter()
        .collect();
    row
}

/// `key`'s thread table, its one row `row`.
fn table(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, key: WorkerKey, row: ThreadRow) {
    let table = TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows: vec![row] };
    view.update_in(cx, |v, _window, cx| v.thread_table(key, &table, cx));
    cx.run_until_parked();
}

fn thread_on(session: SessionId) -> ThreadState {
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    state
}

/// A yes or no on the agent's thread rides the agent's note: "Allow" on the note answers it
/// once and moves nothing, a second press sends nothing, and its row's "Deny" and "Allow" under
/// *Needs you* answer the next ones without going to the agent. A question never gets the
/// buttons.
#[gpui::test]
fn an_approval_is_answered_from_the_note_and_its_row_where_they_are(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (session, other) = (SessionId::new(), SessionId::new());
    let key = WorkerKey::new(7);
    let (tiles, mut link) = worker(&view, cx, key, "mini", &[session, other]);
    view.update_in(cx, |v, _window, cx| {
        v.focus_tile(tiles[1], cx);
        v.agent_event(blocked_on(session), cx);
        v.threads_linked(key, cx);
    });
    let state = thread_on(session);
    let thread = state.meta.id;

    table(&view, cx, key, asking_row(&state, Some(("q-1", Request::QUESTION))));
    let none = view.update(cx, |v, _| v.attention_look().asking[0].approval.clone());
    assert_eq!(none, None, "a question waits for the thread");

    table(&view, cx, key, asking_row(&state, Some(("ask-8", Request::APPROVAL))));
    thread_sent(&mut link);
    let tap = view.update(cx, |v, _| {
        let look = v.attention_look();
        let [asks] = look.asking.as_slice() else { panic!("one agent asks: {look:?}") };
        assert_eq!(asks.approval.as_deref(), Some("ask-8"), "the note answers the request");
        let note = asks.note(false);
        Tap { id: note.id, info: note.info, action: Some(notify::ALLOW.to_owned()), text: None }
    });
    view.update_in(cx, |v, _window, cx| v.open_notification(&tap, cx));
    assert_eq!(intents(&thread_sent(&mut link)), [allowed("ask-8")], "allowed once");
    view.update_in(cx, |v, _window, cx| {
        assert_eq!(v.focused(), Some(tiles[1]), "the answer moved nothing");
        v.open_notification(&tap, cx);
    });
    assert!(intents(&thread_sent(&mut link)).is_empty(), "a second press sends nothing");

    table(&view, cx, key, asking_row(&state, Some(("ask-9", Request::APPROVAL))));
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    cx.run_until_parked();
    press(cx, leak(format!("nav-deny-{thread}")));
    assert_eq!(intents(&thread_sent(&mut link)), [denied("ask-9")], "denied from its row");
    view.update(cx, |v, _cx| {
        assert_eq!(v.focused(), Some(tiles[1]), "without going to the agent");
        assert!(v.session_answer(session).is_none(), "answered");
    });

    table(&view, cx, key, asking_row(&state, Some(("ask-10", Request::APPROVAL))));
    cx.run_until_parked();
    press(cx, leak(format!("nav-allow-{thread}")));
    assert_eq!(intents(&thread_sent(&mut link)), [allowed("ask-10")], "allowed from its row");
    assert!(
        cx.debug_bounds(leak(format!("nav-allow-{thread}"))).is_none(),
        "answered, the row has no buttons"
    );
}

/// A *Needs you* row that has the keyboard answers as its buttons do: ⌘⌫ denies and ⌘↵
/// allows. With the keyboard anywhere else the two keys answer nothing.
#[gpui::test]
fn a_focused_needs_you_row_answers_by_its_keys(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.update(|_w, cx| cx.bind_keys(crate::workspace::key_bindings()));
    let (session, other) = (SessionId::new(), SessionId::new());
    let key = WorkerKey::new(7);
    let (tiles, mut link) = worker(&view, cx, key, "mini", &[session, other]);
    view.update_in(cx, |v, _window, cx| {
        v.focus_tile(tiles[1], cx);
        v.agent_event(blocked_on(session), cx);
        v.threads_linked(key, cx);
    });
    let state = thread_on(session);
    table(&view, cx, key, asking_row(&state, Some(("ask-1", Request::APPROVAL))));
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    cx.run_until_parked();
    drop(thread_sent(&mut link));
    cx.simulate_keystrokes("cmd-enter");
    cx.simulate_keystrokes("cmd-backspace");
    assert!(intents(&thread_sent(&mut link)).is_empty(), "the row does not have the keyboard");

    let row = leak(format!("nav-waiting-{session}"));
    let on_row = |cx: &mut VisualTestContext| {
        let row = cx.debug_bounds(row).expect("the row");
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let nodes = cx.update(|window, _cx| crate::a11y::tree(window));
        let focused = nodes.into_iter().find(|n| n.focused);
        focused.is_some_and(|n| {
            let [x, y, ..] = n.bounds;
            (x - f32::from(row.origin.x)).abs() < 1.0 && (y - f32::from(row.origin.y)).abs() < 1.0
        })
    };
    for _ in 0..80 {
        if on_row(cx) {
            break;
        }
        cx.update(|window, cx| crate::a11y::step(true, window, cx));
        cx.run_until_parked();
    }
    assert!(on_row(cx), "the keyboard's ring reaches the row");
    cx.simulate_keystrokes("cmd-backspace");
    assert_eq!(intents(&thread_sent(&mut link)), [denied("ask-1")], "denied by its key");

    table(&view, cx, key, asking_row(&state, Some(("ask-2", Request::APPROVAL))));
    cx.run_until_parked();
    assert!(on_row(cx), "the row keeps the keyboard");
    cx.simulate_keystrokes("cmd-enter");
    assert_eq!(intents(&thread_sent(&mut link)), [allowed("ask-2")], "allowed by its key");
}

/// The person looking at the agent's terminal, its TUI shown, with the app in front gets the
/// agent's own dialog at once: the request is handed back rather than held for a button. With
/// the app away, it waits for the note's buttons.
#[gpui::test]
fn a_request_whose_terminal_is_in_front_goes_back_to_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let session = SessionId::new();
    let key = WorkerKey::new(7);
    let (tiles, mut link) = worker(&view, cx, key, "mini", &[session]);
    view.update_in(cx, |v, _window, cx| {
        v.agent_event(blocked_on(session), cx);
        v.threads_linked(key, cx);
        v.focus_tile(tiles[0], cx);
        v.show_face(session, false, cx);
        v.set_app_active(false, cx);
    });
    let state = thread_on(session);
    table(&view, cx, key, asking_row(&state, Some(("ask-3", Request::APPROVAL))));
    assert!(intents(&thread_sent(&mut link)).is_empty(), "away, it waits");
    view.update(cx, |v, _cx| assert!(v.session_answer(session).is_some(), "for the note"));
    view.update_in(cx, |v, _window, cx| v.set_app_active(true, cx));
    cx.run_until_parked();
    let release = |ask: &str| Intent::Release { ask: AskId(ask.to_owned()) };
    assert_eq!(intents(&thread_sent(&mut link)), [release("ask-3")]);
    table(&view, cx, key, asking_row(&state, Some(("ask-4", Request::APPROVAL))));
    assert_eq!(intents(&thread_sent(&mut link)), [release("ask-4")], "handed back");
    view.update(cx, |v, _cx| assert!(v.session_answer(session).is_none(), "and not offered"));
}

/// A note's "Allow" that comes before its request (the tap launched the app, or the link is
/// new and the worker's table has not come) waits for it and answers it once it is here. One
/// whose request never comes says so once the table has had time to come: a toast in front,
/// the app's own note while it is away. One whose worker is not reached gives up after a while.
/// One that names no request settles at once.
#[gpui::test]
fn a_notes_answer_waits_for_its_request(cx: &mut TestAppContext) {
    use crate::workspace::approvals::{HOLD_VERDICT, NOT_REACHED, SYNCED};
    let (view, cx) = workspace(cx);
    let session = SessionId::new();
    let key = WorkerKey::new(7);
    let (tiles, mut link) = worker(&view, cx, key, "mini", &[session]);
    view.update_in(cx, |v, _window, cx| {
        v.agent_event(blocked_on(session), cx);
        v.threads_linked(key, cx);
    });
    let state = thread_on(session);
    table(&view, cx, key, asking_row(&state, None));
    thread_sent(&mut link);
    let route = Route { worker: key, item: Some(tiles[0].item), about: About::Session(session) };
    let tap = |route: Route, ask: &str, action: &str| {
        let mut info = route.info();
        info.insert(ASK.to_owned(), ask.to_owned());
        Tap { id: route.about.note_id(), info, action: Some(action.to_owned()), text: None }
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

    view.update_in(cx, |v, _window, cx| {
        v.open_notification(&tap(route, "ask-8", notify::ALLOW), cx);
    });
    cx.run_until_parked();
    assert!(intents(&thread_sent(&mut link)).is_empty(), "the request is not here yet: it waits");
    assert_eq!(toast(cx), None, "nothing said yet");
    table(&view, cx, key, asking_row(&state, Some(("ask-8", Request::APPROVAL))));
    assert_eq!(intents(&thread_sent(&mut link)), [allowed("ask-8")], "answered as it came");
    assert_eq!(
        events.borrow().last(),
        Some(&WorkspaceEvent::TapsSettled),
        "out: an app woken for it may sleep again"
    );
    events.borrow_mut().clear();

    view.update_in(cx, |v, _window, cx| {
        v.open_notification(&tap(route, "ask-9", notify::DENY), cx);
    });
    cx.run_until_parked();
    assert_eq!(toast(cx), None, "still in time for the table");
    cx.executor().advance_clock(SYNCED);
    cx.run_until_parked();
    assert_eq!(toast(cx).as_deref(), Some(NO_LONGER_WAITING), "the table came without it");
    assert_eq!(intents(&thread_sent(&mut link)), []);

    view.update_in(cx, |v, _window, cx| {
        v.set_app_active(false, cx);
        v.open_notification(&tap(route, "ask-10", notify::ALLOW), cx);
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
    view.update_in(cx, |v, _window, cx| v.open_notification(&tap(far, "ask-1", notify::ALLOW), cx));
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

    // A button on a note that names no request answers nothing, and settles at once: the
    // system waits on the app's word that it is done with the tap.
    events.borrow_mut().clear();
    let bare = Tap {
        id: "stray".to_owned(),
        info: route.info(),
        action: Some(notify::DENY.into()),
        text: None,
    };
    view.update_in(cx, |v, _window, cx| v.open_notification(&bare, cx));
    cx.run_until_parked();
    assert_eq!(events.borrow().as_slice(), [WorkspaceEvent::TapsSettled]);
    assert_eq!(intents(&thread_sent(&mut link)), []);
}

/// Claude Code waiting on a yes or no in `session`.
fn blocked_on(session: SessionId) -> AgentEvent {
    AgentEvent {
        session,
        status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
        agent_session: None,
        detail: Some("$ cargo test".into()),
        attention: true,
        source: AgentSource::Hook,
        since_ms: WallMs::ZERO,
        mode: None,
    }
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
    let look = Look {
        asking: Vec::new(),
        turns: vec![turn.clone()],
        unread: 1,
        projects: HashMap::new(),
        muted: HashSet::new(),
    };
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
    let sessions: Vec<_> = words
        .iter()
        .filter_map(|w| match w {
            super::super::agents::Step::Session(at) => Some(at.session),
            super::super::agents::Step::Thread(_) => None,
        })
        .collect();
    assert_eq!(sessions, [away]);
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

/// A muted project's moments post nothing, its waits nor its finished turns; another
/// project's still do.
#[test]
fn a_muted_projects_moments_post_nothing() {
    let (mut attention, memory) = attention();
    let (a, b) = (route(1), route(2));
    attention.set_active(false);
    let projects: HashMap<About, String> =
        [(a.about, "repo:/w/atlas".to_owned()), (b.about, "repo:/w/bolt".to_owned())].into();
    let muted: HashSet<String> = ["repo:/w/atlas".to_owned()].into();
    attention.look(&Look {
        asking: vec![asking(a, "Run cargo test"), asking(b, "Run make")],
        turns: Vec::new(),
        unread: 2,
        projects,
        muted,
    });
    let posted = memory.posted();
    assert_eq!(posted.len(), 1, "only the other project's: {posted:?}");
    assert_eq!(posted[0].id, b.about.note_id());
}

/// Back in front, the notes a push put up while the app was away go too: the system lists them,
/// though the app never posted them. A note about an ask still open stays, to be answered from
/// the Notification Centre, and goes once it is answered; the app's own notes go as before.
#[test]
fn back_in_front_stale_pushed_notes_go_and_a_live_ask_stays() {
    let (mut attention, memory) = attention();
    let (live, answered, shell) = (route(1), route(2), route(3));
    attention.set_active(false);
    attention.look(&Look {
        asking: vec![asking(live, "Run cargo test")],
        turns: Vec::new(),
        unread: 1,
        projects: HashMap::new(),
        muted: HashSet::new(),
    });
    let done = Finished {
        command: "make".into(),
        exit: Some(0),
        elapsed: Duration::from_secs(9),
        turn: None,
    };
    attention.command_finished(shell, "api".into(), &done, Duration::from_secs(5));
    let finished = shell.about.note_id();
    // While the app was suspended, the server pushed the asks of the two threads and a project's
    // word; the second was answered elsewhere since.
    memory.push(&live.about.note_id());
    memory.push(&answered.about.note_id());
    memory.push("project-atlas-7");
    memory.clear();

    attention.set_active(true);
    assert_eq!(memory.withdrawn(), [finished], "its own: the command's note goes, the ask stays");
    attention.delivered(&memory.delivered());
    assert_eq!(
        memory.delivered(),
        [live.about.note_id()],
        "the pushed notes that say nothing now go; the live ask stays"
    );

    attention.look(&Look::default());
    assert!(memory.delivered().is_empty(), "answered, the kept ask goes too");
}

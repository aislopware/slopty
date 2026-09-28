//! What notifies on a pocketed phone, against a notifier that shows nothing: the decisions on
//! their own, then a headless workspace's look and a tapped note's way back to its tile.

use std::sync::Arc;

use gpui::{Entity, TestAppContext, VisualTestContext, px, size};
use slopty_core::ClientId;
use slopty_platform::notify::Memory;
use slopty_proto::ClientMsg;
use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, AgentStatus, BlockReason};
use slopty_proto::handshake::HelloAck;
use slopty_proto::items::{Item, ItemOp, ItemSync};
use slopty_proto::server::WorkerCaps;
use slopty_proto::terminal::{SessionState, SessionSummary};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use super::*;
use crate::screen::ScreenFactory;
use crate::workspace::WorkerLink;

fn route(seed: u128) -> Route {
    Route { worker: WorkerKey::new(seed), item: Some(ItemId::new()), session: SessionId::new() }
}

fn asking(route: Route, body: &str) -> Asking {
    Asking { route, title: "api".into(), body: body.into() }
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
        Route::of_tap(&Tap { id: note.id.clone(), info: note.info.clone() }),
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
        caps: WorkerCaps::default(),
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
        started_ms: 0,
        cols: 80,
        rows: 24,
        state: SessionState::Running,
        viewers: 1,
        command: Vec::new(),
        agent: None,
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
                since_ms: 0,
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
        Tap { id: first.to_string(), info: route.info() }
    });
    view.update_in(cx, |v, _window, cx| {
        v.open_notification(&tap, cx);
        assert_eq!(v.focused(), Some(tiles[0]), "the tap went to its tile");
        assert_eq!(v.pending_focus, Some(first), "and gives its shell the keyboard");
    });
}

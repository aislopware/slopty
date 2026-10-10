//! An orchestrator's helpers: rows, not tiles, each opened on demand as one preview tab.

use slopty_core::WorkerId;
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::TaskState;

use super::*;
use crate::project::fixtures::{card, on, project, snapshot, status, task_changed};

/// The orchestrator's tile, by this client; a shell of the person's in a tab of its own, which
/// has the focus; and the project the server holds, whose tasks 1 and 2 have agents in
/// `first` and `second`.
struct Crew {
    fake: Fake,
    worker: WorkerId,
    mine: TileRef,
    first: SessionId,
    second: SessionId,
}

fn crew(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Crew {
    cx.simulate_resize(size(px(1600.0), px(800.0)));
    let worker = WorkerId::new();
    let fake = connect(view, cx, worker.as_uuid().as_u128(), "studio");
    let (orchestrator, first, second) = (SessionId::new(), SessionId::new(), SessionId::new());
    let _lead = opens(view, cx, &fake, orchestrator, fake.me, 1);
    let mine = opens(view, cx, &fake, SessionId::new(), fake.me, 2);
    on_new_tab(view, cx, mine);
    let tasks = vec![
        on(card(1, "Wire the board", TaskState::Running), worker, first),
        on(card(2, "Read the store", TaskState::Running), worker, second),
    ];
    let term = TermRef { worker, session: orchestrator };
    view.update_in(cx, |v, _w, cx| {
        v.projects_part(
            snapshot(10, vec![status(project("board", Some(term)), tasks, vec![])]),
            cx,
        );
        v.focus_tile(mine, cx);
    });
    cx.run_until_parked();
    Crew { fake, worker, mine, first, second }
}

fn pos(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
    tile: TileRef,
) -> slopty_client::layout::Pos {
    pos_of(view, cx, tile)
}

fn tiles(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |v, _| v.layout.tiles().count())
}

fn placed(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> bool {
    view.read_with(cx, |v, _| v.layout.contains(tile))
}

/// Task agents are rows, not tiles: ten of them arriving from elsewhere leave the tiling as it
/// was and the person's focus where it was. One that came before the server named it a task's
/// agent waits alone in a background tab, and leaves once the project says whose it is.
#[gpui::test]
fn task_agents_arriving_take_no_place(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let crew = crew(&view, cx);
    let before = tiles(&view, cx);
    let mut cards = vec![
        on(card(1, "Wire the board", TaskState::Running), crew.worker, crew.first),
        on(card(2, "Read the store", TaskState::Running), crew.worker, crew.second),
    ];
    let mut sessions = vec![crew.first, crew.second];
    for n in 3..=10 {
        let session = SessionId::new();
        cards.push(on(card(n, "More work", TaskState::Running), crew.worker, session));
        sessions.push(session);
    }
    for (n, card) in (11..).zip(cards.into_iter().skip(2)) {
        view.update_in(cx, |v, _w, cx| v.project_update(n, task_changed("board", card, None), cx));
    }
    cx.run_until_parked();
    for (version, session) in (3..).zip(&sessions) {
        let tile = opens(&view, cx, &crew.fake, *session, ClientId::new(), version);
        assert!(!placed(&view, cx, tile), "a task's agent is a row");
    }
    assert_eq!(tiles(&view, cx), before, "ten task agents, the tiling unchanged");
    assert_eq!(focused(&view, cx), Some(crew.mine), "the focus stays");

    let late = SessionId::new();
    let tile = opens(&view, cx, &crew.fake, late, ClientId::new(), 20);
    assert!(placed(&view, cx, tile), "not known yet: a background tab");
    let named = on(card(11, "Golden files", TaskState::Running), crew.worker, late);
    view.update_in(cx, |v, _w, cx| v.project_update(30, task_changed("board", named, None), cx));
    cx.run_until_parked();
    assert!(!placed(&view, cx, tile), "named: a row like the rest");
    assert_eq!(tiles(&view, cx), before);
    assert_eq!(focused(&view, cx), Some(crew.mine));
}

/// A board row's press opens its task's agent as the helper preview, focused; the next one
/// opened takes its place, so reading through the tasks leaves one tab.
#[gpui::test]
fn a_task_agent_opens_on_demand_as_one_preview_tab(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let crew = crew(&view, cx);
    let first = opens(&view, cx, &crew.fake, crew.first, ClientId::new(), 3);
    let second = opens(&view, cx, &crew.fake, crew.second, ClientId::new(), 4);
    let before = tiles(&view, cx);
    view.update_in(cx, |v, _w, cx| v.reveal_session(crew.first, cx));
    cx.run_until_parked();
    assert!(placed(&view, cx, first), "opened");
    assert_eq!(focused(&view, cx), Some(first));
    assert_eq!(tiles(&view, cx), before.saturating_add(1), "one tab");
    view.update_in(cx, |v, _w, cx| v.reveal_session(crew.second, cx));
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(second));
    assert!(!placed(&view, cx, first), "the next took its place");
    assert_eq!(tiles(&view, cx), before.saturating_add(1), "still one tab");
}

/// A tile the person has given a place keeps it: one shown since it arrived is theirs, and the
/// project naming it later moves nothing.
#[gpui::test]
fn a_tile_the_person_has_seen_stays_where_it_is(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let crew = crew(&view, cx);
    let late = SessionId::new();
    let tile = opens(&view, cx, &crew.fake, late, ClientId::new(), 3);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    let before = pos(&view, cx, tile);
    let named = on(card(3, "Golden files", TaskState::Running), crew.worker, late);
    view.update_in(cx, |v, _w, cx| v.project_update(11, task_changed("board", named, None), cx));
    cx.run_until_parked();
    assert_eq!(pos(&view, cx, tile), before, "where the person had it");
}

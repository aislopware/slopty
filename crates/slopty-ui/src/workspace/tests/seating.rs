//! An orchestrator's helpers: their tiles beside its tile, their rows set in under its row.

use slopty_core::WorkerId;
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::TaskState;

use super::*;
use crate::project::fixtures::{card, on, project, snapshot, status, task_changed};

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

/// The orchestrator's tile, by this client; a shell of the person's in a tab of its own, which
/// has the focus; and the project the server holds, whose tasks 1 and 2 have agents in
/// `first` and `second`.
struct Crew {
    fake: Fake,
    worker: WorkerId,
    lead: TileRef,
    mine: TileRef,
    first: SessionId,
    second: SessionId,
}

fn crew(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Crew {
    cx.simulate_resize(size(px(1600.0), px(800.0)));
    let worker = WorkerId::new();
    let fake = connect(view, cx, worker.as_uuid().as_u128(), "studio");
    let (orchestrator, first, second) = (SessionId::new(), SessionId::new(), SessionId::new());
    let lead = opens(view, cx, &fake, orchestrator, fake.me, 1);
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
    Crew { fake, worker, lead, mine, first, second }
}

fn pos(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
    tile: TileRef,
) -> slopty_client::layout::Pos {
    pos_of(view, cx, tile)
}

/// A task's agent started from elsewhere takes a pane right of its orchestrator, in its tab,
/// and the next shares that pane; the person's focus stays on their own shell. One that came
/// before the server named it a task's agent waits alone in a background tab, and is seated
/// with the rest once the project says whose it is.
#[gpui::test]
fn a_task_agent_opens_beside_its_orchestrator(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let crew = crew(&view, cx);
    let first = opens(&view, cx, &crew.fake, crew.first, ClientId::new(), 3);
    let second = opens(&view, cx, &crew.fake, crew.second, ClientId::new(), 4);
    let lead = pos(&view, cx, crew.lead);
    assert_eq!(pos(&view, cx, first).tab, lead.tab, "in the orchestrator's tab");
    assert_ne!(pos(&view, cx, first).pane, lead.pane, "a pane of its own");
    assert_eq!(pos(&view, cx, second).pane, pos(&view, cx, first).pane, "one column");
    assert_eq!(focused(&view, cx), Some(crew.mine), "the focus stays");

    let late = SessionId::new();
    let third = opens(&view, cx, &crew.fake, late, ClientId::new(), 5);
    assert_ne!(pos(&view, cx, third).tab, lead.tab, "not known yet: a background tab");
    let named = on(card(3, "Golden files", TaskState::Running), crew.worker, late);
    view.update_in(cx, |v, _w, cx| v.project_update(11, task_changed("board", named, None), cx));
    cx.run_until_parked();
    assert_eq!(pos(&view, cx, third).pane, pos(&view, cx, first).pane, "seated with the rest");
    assert_eq!(focused(&view, cx), Some(crew.mine));

    view.update_in(cx, |v, _w, cx| v.focus_tile(crew.lead, cx));
    cx.run_until_parked();
    let x = |cx: &mut VisualTestContext, tile: TileRef| {
        let at = cx.debug_bounds(leak(format!("nav-kind-{}", tile.item.as_uuid())));
        at.unwrap_or_else(|| panic!("{tile:?}'s row")).left()
    };
    let y = |cx: &mut VisualTestContext, tile: TileRef| {
        let at = cx.debug_bounds(leak(format!("nav-tile-{}", tile.item.as_uuid())));
        at.unwrap_or_else(|| panic!("{tile:?}'s row")).top()
    };
    let (lead_x, lead_y) = (x(cx, crew.lead), y(cx, crew.lead));
    for helper in [first, second, third] {
        assert!(x(cx, helper) > lead_x + px(8.0), "set in under the orchestrator's row");
        assert!(y(cx, helper) > lead_y, "after it");
    }
    assert!((x(cx, crew.mine) - lead_x).abs() < px(0.5), "the person's shell is not");
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

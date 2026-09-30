//! A project's board in its orchestrator's tile: turned to and from with ⇧⌘J, kept in step
//! with the server's changes, and every row a way to its agent's tile.

use slopty_core::WorkerId;
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{Moment, ProjectUpdate, TaskId, TaskState};

use super::*;
use crate::project::fixtures::{self, card, entry, on, project, snapshot, status, task_changed};
use crate::project::{Lens, ProjectView};

/// A worker whose key is the one its server id maps to, as the app's are.
fn connect_as(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    id: WorkerId,
    name: &str,
) -> Fake {
    connect(view, cx, id.as_uuid().as_u128(), name)
}

/// Claude Code at work in `session`.
fn working(session: SessionId) -> AgentEvent {
    AgentEvent { status: AgentStatus::Working, attention: false, ..blocked(session) }
}

/// The orchestrator's tile and one agent's, on one worker, and the project the server holds:
/// task 1 runs in the agent's tile, task 2 waits on the person, task 3 waits on task 1.
struct Setup {
    fake: Fake,
    orchestrator: (TileRef, SessionId),
    agent: (TileRef, SessionId),
    blocked: SessionId,
}

fn setup(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Setup {
    let worker = WorkerId::new();
    let fake = connect_as(view, cx, worker, "studio");
    let (orchestrator, agent, blocked_session) =
        (SessionId::new(), SessionId::new(), SessionId::new());
    let orchestrator_tile = opens(view, cx, &fake, orchestrator, fake.me, 1);
    let agent_tile = opens(view, cx, &fake, agent, fake.me, 2);
    let mut after = card(3, "Golden files", TaskState::Planned, Some(1));
    after.depends_on = vec![TaskId(1)];
    let tasks = vec![
        on(card(1, "Wire the board", TaskState::Running, None), worker, agent),
        on(card(2, "Read the store", TaskState::Blocked, None), worker, blocked_session),
        after,
    ];
    let timeline = vec![
        entry(1, None, Moment::Created),
        entry(2, Some(1), Moment::TaskCreated { title: "Wire the board".into() }),
        entry(3, Some(2), Moment::State { from: TaskState::Running, to: TaskState::Blocked }),
    ];
    let term = TermRef { worker, session: orchestrator };
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(working(orchestrator), cx);
        v.agent_event(working(agent), cx);
        v.projects_part(
            snapshot(10, vec![status(project("board", Some(term)), tasks, timeline)]),
            cx,
        );
        v.focus_tile(orchestrator_tile, cx);
    });
    cx.run_until_parked();
    Setup {
        fake,
        orchestrator: (orchestrator_tile, orchestrator),
        agent: (agent_tile, agent),
        blocked: blocked_session,
    }
}

fn board(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
    session: SessionId,
) -> Entity<ProjectView> {
    view.read_with(cx, |v, _| v.board_view(session).cloned()).expect("a board")
}

fn shown(view: &Entity<WorkspaceView>, cx: &VisualTestContext, session: SessionId) -> bool {
    view.read_with(cx, |v, _| v.board_shown(session))
}

/// ⇧⌘J turns the orchestrator's tile to its board, which takes the keyboard: the header, what
/// waits on the person, and the tree. ↓ and ↩ open a task's agent in its own tile; back on the
/// orchestrator the board has the keyboard again, and ⇧⌘J gives the terminal back.
#[gpui::test]
fn the_orchestrators_tile_turns_to_its_board_and_opens_its_agents(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (orchestrator_tile, orchestrator) = setup.orchestrator;
    let (agent_tile, agent) = setup.agent;
    assert!(!shown(&view, cx, orchestrator), "the terminal is the default");

    cx.simulate_keystrokes("cmd-shift-j");
    cx.run_until_parked();
    assert!(shown(&view, cx, orchestrator));
    assert!(cx.debug_bounds("project").is_some(), "the board is drawn in the tile");
    let bounds = |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).is_some();
    for part in [
        "project-title",
        "project-progress",
        "project-bar",
        "project-needs-you",
        "project-needs-project-node-2",
        "project-row-project-node-orchestrator",
        "project-row-project-node-1",
        "project-row-project-node-3",
    ] {
        assert!(bounds(cx, part), "{part} is drawn");
    }
    let tile_bounds = view.read_with(cx, |v, _| v.tile_bounds(orchestrator_tile)).expect("drawn");
    let row = cx.debug_bounds("project-row-project-node-3").expect("drawn");
    assert!(tile_bounds.contains(&row.origin), "in the orchestrator's tile");
    let depth =
        |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).expect("drawn").origin.x;
    assert!(
        depth(cx, "project-row-project-node-3") >= depth(cx, "project-row-project-node-1"),
        "a subtask is drawn under its parent"
    );
    let b = board(&view, cx, orchestrator);
    let board_focused = |cx: &mut VisualTestContext, b: &Entity<ProjectView>| {
        cx.update(|window, cx| b.read(cx).focus_handle(cx).is_focused(window))
    };
    assert!(board_focused(cx, &b), "the board takes the keyboard");
    assert_eq!(focused(&view, cx), Some(orchestrator_tile));

    cx.simulate_keystrokes("down down enter");
    cx.run_until_parked();
    assert_eq!(b.read_with(cx, |b, _| b.picked()), Some(Some(TaskId(1))));
    assert_eq!(focused(&view, cx), Some(agent_tile), "↩ went to task 1's agent");
    assert!(terminal_focused(&view, cx, agent), "with the keyboard in its terminal");

    view.update_in(cx, |v, _w, cx| v.focus_tile(orchestrator_tile, cx));
    cx.run_until_parked();
    assert!(board_focused(cx, &b), "back on the orchestrator, the board has the keyboard");
    cx.simulate_keystrokes("2");
    cx.run_until_parked();
    assert_eq!(b.read_with(cx, |b, _| b.lens()), Lens::Board, "the board had the keyboard");
    for lane in ["needs-you", "working", "up-next"] {
        let lane_id: &'static str = Box::leak(format!("project-lane-{lane}").into_boxed_str());
        assert!(bounds(cx, lane_id), "the {lane} lane");
    }
    cx.simulate_keystrokes("3");
    cx.run_until_parked();
    assert_eq!(b.read_with(cx, |b, _| b.lens()), Lens::Timeline);
    assert!(bounds(cx, "project-entry-3"));

    cx.simulate_keystrokes("cmd-shift-j");
    cx.run_until_parked();
    assert!(!shown(&view, cx, orchestrator));
    assert!(cx.debug_bounds("project").is_none());
    assert!(terminal_focused(&view, cx, orchestrator), "the terminal takes the keyboard back");
    drop(setup.fake);
}

/// A click on a row opens its agent; a row whose agent has no tile here says so rather than
/// going nowhere, and the orchestrator's own row turns the tile back to its terminal.
#[gpui::test]
fn a_click_opens_a_nodes_agent_or_says_why_not(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (agent_tile, _) = setup.agent;
    view.update_in(cx, |v, _w, cx| v.show_board(orchestrator, true, cx));
    cx.run_until_parked();

    let row = cx.debug_bounds("project-row-project-node-1").expect("drawn").center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(agent_tile));

    view.update_in(cx, |v, _w, cx| v.focus_tile(setup.orchestrator.0, cx));
    cx.run_until_parked();
    let row = cx.debug_bounds("project-row-project-node-2").expect("drawn").center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    let notice = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(
        notice.as_deref(),
        Some("#2's agent has no tile here"),
        "no tile for {:?}",
        setup.blocked
    );

    let row = cx.debug_bounds("project-row-project-node-3").expect("drawn").center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()).as_deref(), Some("#3 has no agent yet"));

    let row = cx.debug_bounds("project-row-project-node-orchestrator").expect("drawn").center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    assert!(!shown(&view, cx, orchestrator), "its own row is the way back to the terminal");
}

/// The board follows the server: a change after the snapshot lands, one the snapshot held is
/// dropped, and the board is drawn again only for what it shows. What the window shows after
/// each is what the same state drawn from scratch shows.
#[gpui::test]
fn the_board_follows_the_servers_changes(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (_, agent) = setup.agent;
    let worker = fixtures_worker(&view, cx, orchestrator);
    view.update_in(cx, |v, _w, cx| v.show_board(orchestrator, true, cx));
    cx.run_until_parked();
    let fresh = |cx: &mut VisualTestContext, step: &str| {
        cx.run_until_parked();
        let stale = cx.update(|window, cx| crate::retained::stale(window, cx, 12));
        assert!(stale.is_none(), "{step}: the window shows a stale frame. {stale:?}");
    };
    fresh(cx, "the board");
    let b = board(&view, cx, orchestrator);
    let renders = b.read_with(cx, |b, _| b.renders());

    let old = task_changed(
        "board",
        on(card(1, "Wire the board", TaskState::Merged, None), worker, agent),
        None,
    );
    view.update_in(cx, |v, _w, cx| v.project_update(9, old, cx));
    cx.run_until_parked();
    assert_eq!(
        b.read_with(cx, |b, _| b.renders()),
        renders,
        "a change the snapshot held draws nothing"
    );

    let now_blocked = task_changed(
        "board",
        on(card(1, "Wire the board", TaskState::Blocked, None), worker, agent),
        Some(entry(4, Some(1), Moment::State { from: TaskState::Running, to: TaskState::Blocked })),
    );
    view.update_in(cx, |v, _w, cx| v.project_update(11, now_blocked, cx));
    fresh(cx, "task 1 waits on the person");
    assert!(cx.debug_bounds("project-needs-project-node-1").is_some(), "it joins what needs you");
    assert!(b.read_with(cx, |b, _| b.renders()) > renders);

    // Its agent's own word says what it asks.
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { detail: Some("Which store?".into()), ..blocked(agent) }, cx);
    });
    fresh(cx, "its agent asks");
    let asks = b.read_with(cx, |b, _| b.seen().agents.get(&agent).and_then(|a| a.asks.clone()));
    assert!(asks.is_some(), "the board hears what the agent asks");

    let gone = ProjectUpdate {
        project: fixtures::id("board"),
        record: Some(project("board", None)),
        task: None,
        native: None,
        entry: None,
    };
    view.update_in(cx, |v, _w, cx| v.project_update(12, gone, cx));
    fresh(cx, "the orchestrator let go");
    assert!(
        !shown(&view, cx, orchestrator),
        "a tile that no longer orchestrates shows its terminal"
    );
    assert!(cx.debug_bounds("project").is_none());
}

/// Letting the server go takes its projects, their boards and everything kept for them.
#[gpui::test]
fn forgetting_the_server_keeps_nothing_of_its_projects(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    view.update_in(cx, |v, _w, cx| v.show_board(orchestrator, true, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("project").is_some());
    view.update_in(cx, |v, _w, cx| v.forget_projects(cx));
    cx.run_until_parked();
    let kept: Vec<(&str, usize)> = view
        .read_with(cx, |v, _| v.project_sizes().to_vec())
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .collect();
    assert!(kept.is_empty(), "left behind: {kept:?}");
    assert!(view.read_with(cx, |v, _| v.projects().is_empty()));
    assert!(cx.debug_bounds("project").is_none());
}

/// The palette lists each project; ↩ on it shows its board in the orchestrator's tile and
/// goes there. From an agent on a task, ⇧⌘J goes to its project's board.
#[gpui::test]
fn the_palette_and_an_agent_reach_the_board(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (orchestrator_tile, orchestrator) = setup.orchestrator;
    let (agent_tile, _) = setup.agent;
    view.update_in(cx, |v, _w, cx| v.focus_tile(agent_tile, cx));
    cx.run_until_parked();
    let lines = view.read_with(cx, |v, _| v.project_lines());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].label, "Ship the project board");
    assert_eq!(lines[0].status, Some(crate::icons::Status::NeedsYou), "its most urgent lane");

    cx.simulate_keystrokes("cmd-shift-j");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(orchestrator_tile));
    assert!(shown(&view, cx, orchestrator));

    view.update_in(cx, |v, _w, cx| {
        v.show_board(orchestrator, false, cx);
        v.focus_tile(agent_tile, cx);
        v.open_project(&fixtures::id("board"), cx);
    });
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(orchestrator_tile));
    assert!(shown(&view, cx, orchestrator));
}

/// The worker the fixtures put the project on: the orchestrator's.
fn fixtures_worker(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
    orchestrator: SessionId,
) -> WorkerId {
    view.read_with(cx, |v, _| {
        v.projects().of_orchestrator(orchestrator).and_then(|b| b.worker(None)).expect("a worker")
    })
}

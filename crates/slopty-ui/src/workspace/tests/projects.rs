//! A project's board in its orchestrator's tile: turned to and from with ⇧⌘J, kept in step
//! with the server's changes, and every row a way to its agent's tile.

use slopty_core::WorkerId;
use slopty_proto::orchestration::{Outcome, TermRef, Verb};
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

/// A project's agents are named by what they are to it, not "Claude Code" and "Claude Code 2":
/// the orchestrator, and each task's agent by its task's number and title.
#[gpui::test]
fn a_project_s_agents_are_named_by_their_part_in_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (orchestrator_tile, _) = setup.orchestrator;
    let (agent_tile, _) = setup.agent;
    let title = |cx: &mut VisualTestContext, tile: TileRef| {
        view.read_with(cx, |v, _| v.tile_title(v.item(tile).expect("a tile")))
    };
    assert_eq!(title(cx, orchestrator_tile), "Orchestrator");
    assert_eq!(title(cx, agent_tile), "#1 Wire the board");
    let lines = view.read_with(cx, WorkspaceView::navigator_lines);
    assert!(lines.iter().any(|(t, ..)| t == "#1 Wire the board"), "{lines:?}");
    assert!(!lines.iter().any(|(t, ..)| t.starts_with("Claude Code")), "{lines:?}");
    drop(setup.fake);
}

/// An agent opened from a board that fills the view comes in beside it: the board's column
/// gives up its full width, so neither is left cut off at the window's edge.
#[gpui::test]
fn an_agent_opened_from_a_full_width_board_shows_beside_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (orchestrator_tile, orchestrator) = setup.orchestrator;
    let (agent_tile, _) = setup.agent;
    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
    let full = |view: &Entity<WorkspaceView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| {
            let pos = v.layout.position(orchestrator_tile).expect("placed");
            v.layout.workspaces()[pos.workspace].columns()[pos.column].is_full_width()
        })
    };
    assert!(full(&view, cx), "the board fills the view");
    view.update_in(cx, |v, _w, cx| v.show_board(orchestrator, true, cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("down down enter");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(agent_tile), "↩ went to task 1's agent");
    assert!(!full(&view, cx), "the board gave up its full width");
    let (board, agent, strip) = view.read_with(cx, |v, _| {
        (v.tile_bounds(orchestrator_tile), v.tile_bounds(agent_tile), v.drawn.viewport.get())
    });
    let (board, agent) = (board.expect("the board shows"), agent.expect("the agent shows"));
    assert!(board.left() >= strip.left() - px(0.5), "the board is whole: {board:?} in {strip:?}");
    assert!(agent.right() <= strip.right() + px(0.5), "{agent:?} in {strip:?}");
    drop(setup.fake);
}

/// A board taller than its tile says so: its foot fades into the tile while lanes run on
/// below, and its top once it is scrolled; at the end the foot is clear. A card's action is a
/// button a click can find, as tall as the least a click needs.
#[gpui::test]
fn a_board_taller_than_its_tile_says_more_lies_below(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let worker = WorkerId::new();
    let fake = connect_as(&view, cx, worker, "studio");
    let orchestrator = SessionId::new();
    let _tile = opens(&view, cx, &fake, orchestrator, fake.me, 1);
    let mut tasks: Vec<_> =
        (1..=24).map(|n| card(n, "Planned work", TaskState::Planned, None)).collect();
    tasks.push(card(25, "Ship it", TaskState::Done, None));
    let term = TermRef { worker, session: orchestrator };
    view.update_in(cx, |v, _w, cx| {
        v.projects_part(
            snapshot(10, vec![status(project("board", Some(term)), tasks, vec![])]),
            cx,
        );
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    board(&view, cx, orchestrator).update(cx, |b, cx| b.show(Lens::Board, cx));
    cx.run_until_parked();
    let faded = |cx: &mut VisualTestContext| {
        // The fade reads the body's extent as it lays out, so it lands a frame later.
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
        let body = cx.debug_bounds("project-body").expect("drawn");
        cx.update(|window, _| crate::retained::faded_edges(window, body))
    };
    let edges = faded(cx);
    assert!(edges.bottom, "lanes run on below");
    assert!(!edges.top, "nothing above yet");

    let merge = cx.debug_bounds("project-card-merge-25").expect("Ship it can merge");
    assert!(
        merge.size.height >= px(Theme::default().density.hit) - px(0.5),
        "a button's height, not a line of text's: {merge:?}"
    );

    let at = cx.debug_bounds("project-body").expect("drawn").center();
    cx.simulate_event(ScrollWheelEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(0.0), px(-100_000.0))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        momentum_phase: None,
    });
    cx.run_until_parked();
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let edges = faded(cx);
    assert!(edges.top, "scrolled: what is above fades");
    assert!(!edges.bottom, "at the end nothing lies below");
    drop(fake);
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
    let (view, cx) = still_workspace(cx);
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

/// What waits on the person stands on the tree's column and says no word its heading says;
/// the lanes share the tile's width equally, however many there are; a task made under the
/// title it still has says only that it was made; a run of entries of one age says it once.
#[gpui::test]
fn the_board_says_each_thing_once_and_fills_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    view.update_in(cx, |v, _w, cx| v.show_board(orchestrator, true, cx));
    cx.run_until_parked();
    cx.update(|window, _cx| window.set_a11y_active(true));
    let labels = |view: &Entity<WorkspaceView>, cx: &mut VisualTestContext| -> Vec<String> {
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        tree.into_iter().filter_map(|n| n.label).collect()
    };
    let x =
        |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).expect("drawn").origin.x;
    assert_eq!(
        x(cx, "project-needs-project-node-2"),
        x(cx, "project-row-project-node-1"),
        "the band's rows on the tree's column"
    );
    let said = labels(&view, cx);
    let store: Vec<&String> = said.iter().filter(|l| l.starts_with("Read the store")).collect();
    assert_eq!(store.len(), 2, "in the band and in the tree: {said:?}");
    assert_eq!(
        store.iter().filter(|l| l.contains("Needs you")).count(),
        1,
        "the tree's row says it, the band's leaves it to its heading: {store:?}"
    );

    let b = board(&view, cx, orchestrator);
    b.update(cx, |b, cx| b.show(Lens::Board, cx));
    cx.run_until_parked();
    let body = cx.debug_bounds("project-body").expect("drawn");
    let lanes: Vec<Bounds<Pixels>> = ["needs-you", "working", "up-next"]
        .map(|lane| {
            let id: &'static str = Box::leak(format!("project-lane-{lane}").into_boxed_str());
            cx.debug_bounds(id).expect("drawn")
        })
        .into();
    let width = lanes[0].size.width;
    assert!(
        lanes.iter().all(|l| (l.size.width - width).abs() < px(0.5)),
        "one width for every lane: {lanes:?}"
    );
    let right = lanes.iter().map(Bounds::right).fold(px(0.0), Pixels::max);
    assert!(
        body.right() - right <= px(Theme::default().spacing.inset()),
        "no lane-wide gap at the right: {lanes:?} in {body:?}"
    );
    assert!(
        cx.debug_bounds("project-needs-project-node-2").is_none(),
        "the board's own lane says what needs you, not the band over it too"
    );

    b.update(cx, |b, cx| b.show(Lens::Timeline, cx));
    let said = labels(&view, cx);
    assert!(
        said.iter().any(|l| l.starts_with("#1 Wire the board: Created,")),
        "made under the title it has: {said:?}"
    );
    let drawn = |cx: &mut VisualTestContext, s: String| {
        cx.debug_bounds(Box::leak(s.into_boxed_str())).is_some()
    };
    let entries: Vec<u64> =
        (0..64).filter(|seq| drawn(cx, format!("project-entry-{seq}"))).collect();
    let aged: Vec<u64> = entries
        .iter()
        .copied()
        .filter(|seq| drawn(cx, format!("project-entry-{seq}-age")))
        .collect();
    assert!(entries.len() > 1, "a timeline to read: {entries:?}");
    assert_eq!(
        aged,
        entries.iter().max().copied().into_iter().collect::<Vec<_>>(),
        "entries of one age say it once, on the newest"
    );
}

/// A verifier shows where its task does. In the tree, a run under way or a failure stands
/// under its row, with the failure's last lines, and a pass is a word in the row. On the
/// board, every card says its verdict and the commits judged, and Ready to merge runs in the
/// queue's order. "Output" opens the verifier's terminal without opening the agent. A terminal
/// that has closed says so.
#[gpui::test]
fn a_verifier_shows_on_its_task_and_opens_its_terminal(cx: &mut TestAppContext) {
    use slopty_proto::project::{StepKind, StepState};

    use crate::project::fixtures::{queued, run, step};
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (orchestrator_tile, orchestrator) = setup.orchestrator;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let kept = SessionId::new();
    let kept_tile = opens(&view, cx, &setup.fake, kept, setup.fake.me, 3);
    let closed = SessionId::new();

    let mut failed = card(4, "Read the snapshot", TaskState::Waiting, None);
    failed.verified = Some(run(false, "4a7aa6d0"));
    let why = StepState::Failed { why: "exit 101".into() };
    failed.step =
        Some(step(StepKind::Verify, worker, why, Some(TermRef { worker, session: kept })));
    let mut running = card(5, "Draw the lanes", TaskState::Verifying, None);
    let line = StepState::Running { phase: "Compiling slopty-ui".into(), percent: None };
    running.step =
        Some(step(StepKind::Verify, worker, line, Some(TermRef { worker, session: closed })));
    let later = queued(card(6, "Hold it to goldens", TaskState::Done, None), 20, "6666666");
    let sooner = queued(card(7, "Write the decision", TaskState::Done, None), 10, "7777777");
    view.update_in(cx, |v, _w, cx| {
        for (seq, c) in [(11, failed), (12, running), (13, later), (14, sooner)] {
            v.project_update(seq, task_changed("board", c, None), cx);
        }
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    let drawn = |cx: &mut VisualTestContext, s: &str| {
        cx.debug_bounds(Box::leak(s.to_owned().into_boxed_str())).is_some()
    };
    for part in [
        "project-row-project-node-4-check",
        "project-row-project-node-4-tail",
        "project-row-project-node-4-output",
        "project-row-project-node-5-check",
    ] {
        assert!(drawn(cx, part), "{part} is drawn");
    }
    assert!(!drawn(cx, "project-row-project-node-6-check"), "a pass is a word in its row");
    assert!(!drawn(cx, "project-row-project-node-5-tail"), "a run under way has no last lines");

    let link = cx.debug_bounds("project-row-project-node-4-output").expect("drawn").center();
    cx.simulate_click(link, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(kept_tile), "the verifier's terminal, not the agent's");
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()), None, "the row's own click held back");

    view.update_in(cx, |v, _w, cx| v.focus_tile(orchestrator_tile, cx));
    cx.run_until_parked();
    let link = cx.debug_bounds("project-row-project-node-5-output").expect("drawn").center();
    cx.simulate_click(link, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("The verifier's terminal has closed")
    );
    assert_eq!(focused(&view, cx), Some(orchestrator_tile), "and nothing else opened");

    let b = board(&view, cx, orchestrator);
    b.update(cx, |b, cx| b.show(Lens::Board, cx));
    cx.run_until_parked();
    for part in ["project-card-4-tail", "project-card-6-check", "project-card-7-check"] {
        assert!(drawn(cx, part), "{part} is drawn");
    }
    let y =
        |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).expect("drawn").origin.y;
    assert!(
        y(cx, "project-card-7") < y(cx, "project-card-6"),
        "the one waiting longest is next to merge"
    );
}

/// A reviewer shows where its task does, as a verifier does. At work, it stands under its
/// row with "Reviewer" to open its session. Its changes asked stand under the row with what it
/// found, what blocks first, and the session it is kept in opens without opening the agent.
/// An approval is a word in the row, and on the board a card says it with its note. A session
/// that has closed says so.
#[gpui::test]
fn a_reviewer_shows_on_its_task_and_opens_its_session(cx: &mut TestAppContext) {
    use slopty_proto::project::{StepKind, StepState};

    use crate::project::fixtures::{queued, review, run, step};
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (orchestrator_tile, orchestrator) = setup.orchestrator;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let kept = SessionId::new();
    let kept_tile = opens(&view, cx, &setup.fake, kept, setup.fake.me, 3);
    let closed = SessionId::new();
    let reviewer = TermRef { worker, session: kept };

    let mut asked = card(4, "Read the snapshot", TaskState::Waiting, None);
    asked.verified = Some(run(true, "4a7aa6d0"));
    asked.reviewed = Some(review(false, "4a7aa6d0", Some(reviewer)));
    let why = StepState::Failed { why: "Project.review has no golden".into() };
    asked.step = Some(step(StepKind::Review, worker, why, Some(reviewer)));
    let mut reading = card(5, "Draw the lanes", TaskState::Verifying, None);
    let line = StepState::Running { phase: "Reading 5555555 over c08d4c1".into(), percent: None };
    reading.step =
        Some(step(StepKind::Review, worker, line, Some(TermRef { worker, session: closed })));
    let mut approved = queued(card(6, "Hold it to goldens", TaskState::Done, None), 20, "6666666");
    approved.reviewed = Some(review(true, "6666666", Some(reviewer)));
    view.update_in(cx, |v, _w, cx| {
        for (seq, c) in [(11, asked), (12, reading), (13, approved)] {
            v.project_update(seq, task_changed("board", c, None), cx);
        }
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    let drawn = |cx: &mut VisualTestContext, s: &str| {
        cx.debug_bounds(Box::leak(s.to_owned().into_boxed_str())).is_some()
    };
    for part in [
        "project-row-project-node-4-check",
        "project-row-project-node-4-findings",
        "project-row-project-node-4-output",
        "project-row-project-node-5-check",
        "project-row-project-node-5-output",
    ] {
        assert!(drawn(cx, part), "{part} is drawn");
    }
    assert!(!drawn(cx, "project-row-project-node-6-check"), "an approval is a word in its row");
    assert!(!drawn(cx, "project-row-project-node-5-findings"), "nothing found while it reads");

    let link = cx.debug_bounds("project-row-project-node-4-output").expect("drawn").center();
    cx.simulate_click(link, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(kept_tile), "the reviewer's session, not the agent's");
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()), None, "the row's own click held back");

    view.update_in(cx, |v, _w, cx| v.focus_tile(orchestrator_tile, cx));
    cx.run_until_parked();
    let link = cx.debug_bounds("project-row-project-node-5-output").expect("drawn").center();
    cx.simulate_click(link, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("The reviewer's session has closed")
    );

    let b = board(&view, cx, orchestrator);
    b.update(cx, |b, cx| b.show(Lens::Board, cx));
    cx.run_until_parked();
    for part in ["project-card-4-findings", "project-card-6-check", "project-card-6-findings"] {
        assert!(drawn(cx, part), "{part} is drawn");
    }
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

/// Every verb the board sent so far, each answered with `answer`.
fn sent(
    queue: &mut slopty_client::server::CallQueue,
    cx: &VisualTestContext,
    answer: impl Fn(&Verb) -> Outcome,
) -> Vec<Verb> {
    cx.run_until_parked();
    let mut verbs = Vec::new();
    while let Some((verb, reply)) = queue.try_next() {
        let _gone = reply.send(answer(&verb));
        verbs.push(verb);
    }
    cx.run_until_parked();
    verbs
}

fn done(_: &Verb) -> Outcome {
    Outcome::Done
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let at = cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str())).expect(selector);
    cx.simulate_click(at.center(), Modifiers::none());
    cx.run_until_parked();
}

/// The board's actions reach the server as the person's word. A finished task's Merge and a
/// changes-asked task's Approve are buttons on its row and its card, which send `TaskMerge`
/// and an approving `TaskReview` without opening the agent; a refusal is said in the server's
/// words. The header's push toggle sets the project's pushing, and its terminal button turns
/// the tile back to the orchestrator. "Delete the project" asks twice. With no server, the
/// board says so.
#[gpui::test]
fn the_boards_actions_reach_the_server(cx: &mut TestAppContext) {
    use slopty_proto::orchestration::ErrorCode;

    use crate::project::fixtures::{review, run};
    use crate::project::{ApproveTask, DeleteProject, MergeTask};
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let mut asked = card(4, "Read the snapshot", TaskState::Waiting, None);
    asked.verified = Some(run(true, "4a7aa6d0"));
    asked.reviewed = Some(review(false, "4a7aa6d0", None));
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.project_update(
            11,
            task_changed("board", card(5, "Land it", TaskState::Done, None), None),
            cx,
        );
        v.project_update(12, task_changed("board", asked, None), cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    let project = fixtures::id("board");

    click(cx, "project-row-merge-5");
    assert_eq!(
        sent(&mut queue, cx, done),
        [Verb::TaskMerge { project: project.clone(), task: TaskId(5) }]
    );
    assert!(shown(&view, cx, orchestrator), "the button held the row's own click back");
    assert!(
        cx.debug_bounds("project-row-merge-1").is_none(),
        "a task at work has nothing to merge"
    );

    let mut unpushed = card(6, "Pushed late", TaskState::Merged, None);
    unpushed.merge = Some(slopty_proto::project::Merge::Merged {
        target: "main".into(),
        head: "abcdef0123".into(),
        at_ms: fixtures::AT,
        pushed: false,
        push_failed: Some("could not read Username".into()),
    });
    view.update_in(cx, |v, _w, cx| v.project_update(13, task_changed("board", unpushed, None), cx));
    cx.run_until_parked();
    click(cx, "project-row-push-again-6");
    assert_eq!(
        sent(&mut queue, cx, done),
        [Verb::TaskPush { project: project.clone(), task: TaskId(6) }]
    );

    click(cx, "project-row-approve-4");
    let refused = |_: &Verb| Outcome::Error {
        code: ErrorCode::Conflict,
        message: "#4 changed under the board".into(),
    };
    let verbs = sent(&mut queue, cx, refused);
    let [Verb::TaskReview { task: TaskId(4), verdict, .. }] = verbs.as_slice() else {
        panic!("an approval: {verbs:?}");
    };
    assert!(verdict.approved && verdict.findings.is_empty(), "{verdict:?}");
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("#4 changed under the board"),
        "a refusal in the server's words"
    );

    let b = board(&view, cx, orchestrator);
    b.update(cx, |b, cx| b.show(Lens::Board, cx));
    cx.run_until_parked();
    for part in ["project-card-merge-5", "project-card-approve-4"] {
        assert!(cx.debug_bounds(Box::leak(part.to_owned().into_boxed_str())).is_some(), "{part}");
    }
    b.update(cx, |b, cx| b.show(Lens::Tree, cx));
    cx.run_until_parked();

    click(cx, "project-push");
    let verbs = sent(&mut queue, cx, done);
    assert!(
        matches!(verbs.as_slice(), [Verb::ProjectSet { push: Some(true), orchestrator: None, .. }]),
        "{verbs:?}"
    );

    // From the keyboard, the action names the task stood on.
    cx.dispatch_action(MergeTask);
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("Stand on a task to merge")
    );
    b.update(cx, |b, cx| {
        b.select_by(1, cx);
        b.select_by(1, cx);
    });
    assert_eq!(b.read_with(cx, |b, _| b.picked()), Some(Some(TaskId(1))));
    cx.dispatch_action(ApproveTask);
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("#1 has nothing to approve")
    );
    assert_eq!(sent(&mut queue, cx, done), []);

    cx.dispatch_action(DeleteProject);
    cx.run_until_parked();
    assert!(sent(&mut queue, cx, done).is_empty(), "the first ask only says what a second does");
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some(
            "Delete again to let Ship the project board go. Its terminals stay; its tasks and \
             timeline do not"
        )
    );
    cx.dispatch_action(DeleteProject);
    assert_eq!(sent(&mut queue, cx, done), [Verb::ProjectDelete { project }]);

    click(cx, "project-terminal");
    assert!(!shown(&view, cx, orchestrator), "the header's way back to the terminal");

    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(None);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    click(cx, "project-row-merge-5");
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some(super::super::projects::NO_SERVER)
    );
}

/// "Start a project here" opens the "New project" sheet for the focused terminal's agent,
/// named for its directory and kept clear of the names taken, its repository and branch
/// filled in; Create makes the project with what the sheet holds and that terminal as its
/// orchestrator, and its board shows once the server's word of it arrives. A plain shell is
/// refused with why, and a terminal that already orchestrates one shows that one.
#[gpui::test]
fn a_project_starts_in_the_focused_terminal(cx: &mut TestAppContext) {
    use crate::project::StartProject;
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (orchestrator_tile, orchestrator) = setup.orchestrator;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    view.update_in(cx, |v, _w, _cx| v.set_server_caller(Some(caller)));
    let here = SessionId::new();
    let tile =
        opens_in(&view, cx, &setup.fake, here, setup.fake.me, 3, Some("/Users/me/src/Board"));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();

    cx.dispatch_action(StartProject);
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some(super::super::projects::NOT_AN_AGENT),
        "a shell hears nothing the board says"
    );
    assert!(cx.debug_bounds("project-sheet").is_none());

    view.update_in(cx, |v, _w, cx| v.agent_event(working(here), cx));
    cx.run_until_parked();
    cx.dispatch_action(StartProject);
    cx.run_until_parked();
    assert!(cx.debug_bounds("project-sheet").is_some(), "the sheet asks first");
    assert_eq!(sent(&mut queue, cx, done), [], "nothing made before Create");
    let typed = view.read_with(cx, WorkspaceView::project_sheet_typed).expect("the sheet");
    assert_eq!(
        (typed.title.as_str(), typed.repo.as_str(), typed.target.as_str(), typed.push),
        ("Board", "/Users/me/src/Board", "main", false)
    );
    click(cx, "project-sheet-push");
    click(cx, "project-sheet-create");
    let verbs = sent(&mut queue, cx, done);
    let [
        Verb::ProjectCreate {
            project,
            title,
            repo,
            target,
            push: true,
            verifier: None,
            orchestrator: Some(term),
            ..
        },
    ] = verbs.as_slice()
    else {
        panic!("a project, pushed: {verbs:?}");
    };
    assert_eq!(project.as_str(), "board-2", "\"board\" is taken");
    assert_eq!(
        (title.as_str(), repo.as_str(), target.as_str()),
        ("Board", "/Users/me/src/Board", "main")
    );
    assert_eq!(*term, TermRef { worker, session: here });
    assert!(cx.debug_bounds("project-sheet").is_none(), "made: the sheet goes");
    assert!(!shown(&view, cx, here), "not before the server's word of it");

    let mut made = fixtures::project("board-2", Some(*term));
    made.title = "Board".into();
    let update = ProjectUpdate {
        project: made.id.clone(),
        record: Some(made),
        task: None,
        native: None,
        entry: None,
    };
    view.update_in(cx, |v, _w, cx| v.project_update(20, update, cx));
    cx.run_until_parked();
    assert!(shown(&view, cx, here), "its board shows once the server has it");
    assert_eq!(focused(&view, cx), Some(tile));

    view.update_in(cx, |v, _w, cx| v.focus_tile(orchestrator_tile, cx));
    cx.run_until_parked();
    cx.dispatch_action(StartProject);
    assert_eq!(sent(&mut queue, cx, done), [], "it already orchestrates one");
    assert!(shown(&view, cx, orchestrator));
}

/// "Make this agent X's orchestrator" is offered for each project the focused agent does not
/// orchestrate, and sets it on the server; the tile shows the board once the server says the
/// agent orchestrates it. A plain shell is offered nothing and refused.
#[gpui::test]
fn an_agent_becomes_a_project_s_orchestrator(cx: &mut TestAppContext) {
    use super::super::actions::MakeOrchestrator;
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    view.update_in(cx, |v, _w, _cx| v.set_server_caller(Some(caller)));
    let here = SessionId::new();
    let tile = opens_in(&view, cx, &setup.fake, here, setup.fake.me, 3, Some("/Users/me/src"));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    let lines = |cx: &mut VisualTestContext| -> Vec<String> {
        view.read_with(cx, |v, _| v.orchestrator_lines().into_iter().map(|l| l.label).collect())
    };
    assert_eq!(lines(cx), Vec::<String>::new(), "a shell is offered nothing");
    let make = MakeOrchestrator { project: fixtures::id("board") };
    view.update_in(cx, |v, w, cx| v.make_orchestrator(&make, w, cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some(super::super::projects::NOT_AN_AGENT)
    );
    assert_eq!(sent(&mut queue, cx, done), []);

    view.update_in(cx, |v, _w, cx| v.agent_event(working(here), cx));
    cx.run_until_parked();
    assert_eq!(lines(cx), ["Make this agent Ship the project board's orchestrator".to_owned()]);
    view.update_in(cx, |v, w, cx| v.make_orchestrator(&make, w, cx));
    let term = TermRef { worker, session: here };
    let verbs = sent(&mut queue, cx, done);
    assert!(
        matches!(
            verbs.as_slice(),
            [Verb::ProjectSet { orchestrator: Some(t), push: None, verifier: None, .. }] if *t == term
        ),
        "{verbs:?}"
    );
    assert!(!shown(&view, cx, here), "not before the server says so");
    let moved = project("board", Some(term));
    let update = ProjectUpdate {
        project: moved.id.clone(),
        record: Some(moved),
        task: None,
        native: None,
        entry: None,
    };
    view.update_in(cx, |v, _w, cx| v.project_update(30, update, cx));
    cx.run_until_parked();
    assert!(shown(&view, cx, here), "the board turns to the new orchestrator's tile");
    assert_eq!(lines(cx), Vec::<String>::new(), "it orchestrates the only one now");
}

/// The task the board stands on offers Stop its agent while one runs and Cancel task until it
/// is merged, after what it waits for and quieter; a task not stood on offers neither. Stop
/// closes its agent's terminal and Cancel gives it up with the person's word, from its button
/// or the palette.
#[gpui::test]
fn a_task_stood_on_can_be_stopped_or_cancelled(cx: &mut TestAppContext) {
    use slopty_proto::project::TaskChange;

    use crate::project::CancelTask;
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (_, agent) = setup.agent;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("project-row-stop-1").is_none(), "only on the task stood on");
    let b = board(&view, cx, orchestrator);
    b.update(cx, |b, cx| {
        b.select_by(1, cx);
        b.select_by(1, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("project-row-cancel-1").is_some());
    assert!(cx.debug_bounds("project-row-stop-3").is_none(), "task 3 is not stood on");
    click(cx, "project-row-stop-1");
    assert_eq!(
        sent(&mut queue, cx, done),
        [Verb::Close { term: TermRef { worker, session: agent } }]
    );
    cx.dispatch_action(CancelTask);
    let change = TaskChange {
        state: Some(TaskState::Failed),
        note: Some(super::super::projects::CANCELLED.to_owned()),
        ..TaskChange::default()
    };
    assert_eq!(
        sent(&mut queue, cx, done),
        [Verb::TaskUpdate {
            project: fixtures::id("board"),
            task: TaskId(1),
            change: Box::new(change)
        }]
    );
}

/// What the server says of a worker: its name, whether it is reached, and the rest.
fn facts(worker: WorkerId, name: &str, online: bool) -> slopty_proto::project::WorkerFacts {
    use slopty_proto::project::Fact;
    let facts = [
        ("name", Fact::Text(name.to_owned())),
        ("online", Fact::Bool(online)),
        ("os", Fact::Text("macos".to_owned())),
        ("cpus", Fact::Int(12)),
        ("memory_mb", Fact::Int(65_536)),
        ("load", Fact::Float(2.14)),
        ("live_agents", Fact::Int(3)),
    ];
    slopty_proto::project::WorkerFacts {
        worker,
        facts: facts.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
    }
}

/// The machines lens asks the server how the workers are doing and draws each with the
/// project's agents on it, the workers in use first and one away last, then the tasks still
/// to start. "Run on…" opens the server's ranking under the task, with why each worker fits or
/// not; a choice pins the task there and closes the picker.
#[gpui::test]
fn the_machines_lens_shows_where_everything_runs_and_where_a_task_will(cx: &mut TestAppContext) {
    use slopty_proto::project::{Reason, RunOn, Suggestion};
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    let b = board(&view, cx, orchestrator);
    b.update(cx, |b, cx| b.show(Lens::Machines, cx));
    let away = WorkerId::new();
    let verbs = sent(&mut queue, cx, |_| {
        Outcome::Facts(vec![facts(away, "attic", false), facts(worker, "studio", true)])
    });
    assert_eq!(verbs, [Verb::WorkerFacts { worker: None }]);

    let y = |cx: &mut VisualTestContext, s: String| {
        cx.debug_bounds(Box::leak(s.into_boxed_str())).map(|b| b.origin.y)
    };
    let studio = y(cx, format!("project-host-{worker}")).expect("the worker in use");
    let attic = y(cx, format!("project-host-{away}")).expect("the worker away");
    assert!(studio < attic, "the worker in use first");
    for node in ["orchestrator", "1", "2"] {
        let row = y(cx, format!("project-machine-project-node-{node}")).expect(node);
        assert!(row > studio && row < attic, "{node} under the worker it runs on");
    }
    let waiting = y(cx, "project-machines-waiting".to_owned()).expect("what is still to start");
    assert!(y(cx, "project-machine-project-node-3".to_owned()).is_some_and(|r| r > waiting));

    click(cx, "project-machine-run-on-3");
    let reason = |rule: &str, held: bool, detail: &str| Reason {
        rule: rule.to_owned(),
        held,
        points: 0,
        detail: detail.to_owned(),
        need: None,
    };
    let ranked = vec![
        Suggestion {
            worker,
            name: "studio".to_owned(),
            fits: true,
            score: 0,
            reasons: vec![reason("online", true, ""), reason(r#"os == "macos""#, true, "")],
        },
        Suggestion {
            worker: away,
            name: "attic".to_owned(),
            fits: false,
            score: 0,
            reasons: vec![reason("online", false, "not online")],
        },
    ];
    let verbs = sent(&mut queue, cx, |_| Outcome::Suggestions(ranked.clone()));
    assert_eq!(
        verbs,
        [Verb::PlacementSuggest {
            project: Some(fixtures::id("board")),
            task: Some(TaskId(3)),
            placement: None
        }]
    );
    for option in ["anywhere", "0", "1"] {
        let id = format!("project-machine-picker-3-{option}");
        assert!(y(cx, id.clone()).is_some(), "{id}");
    }
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let said: Vec<String> = cx
        .update(|window, _cx| crate::a11y::tree(window))
        .into_iter()
        .filter_map(|n| n.label)
        .collect();
    assert!(said.iter().any(|l| l == r#"studio, os == "macos""#), "{said:?}");
    assert!(said.iter().any(|l| l == "attic, not online"), "{said:?}");

    click(cx, "project-machine-picker-3-0");
    let verbs = sent(&mut queue, cx, |_| Outcome::Done);
    let [Verb::TaskUpdate { task: TaskId(3), change, .. }] = verbs.as_slice() else {
        panic!("a pin: {verbs:?}");
    };
    assert_eq!(change.run_on, Some(RunOn::Worker(worker)));
    assert!(y(cx, "project-machine-picker-3".to_owned()).is_none(), "the choice closes it");
}

/// Before it fans out, the plan waits for the person: the band over the lens holds each
/// proposed task with where it would start and why and how long tasks take, and the
/// orchestrator's row says it waits on them. Start starts one, "Start all" every one, a
/// worker chosen in its picker starts it there, and the header's toggle says whether the
/// project asks at all.
#[gpui::test]
fn a_plan_waits_for_the_person_who_starts_one_or_all(cx: &mut TestAppContext) {
    use slopty_proto::project::Proposed;
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let proposed = |n, title| {
        let mut c = card(n, title, TaskState::Planned, None);
        c.proposed = Some(Proposed {
            since_ms: fixtures::AT,
            runs: "claude".to_owned(),
            on: Some(worker),
            why: r#"os == "macos""#.to_owned(),
        });
        c
    };
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.project_update(11, task_changed("board", proposed(4, "Draw the plan"), None), cx);
        v.project_update(12, task_changed("board", proposed(5, "Start it"), None), cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    for part in ["project-plan", "project-plan-project-node-4", "project-plan-project-node-5"] {
        assert!(cx.debug_bounds(part).is_some(), "{part} is drawn");
    }
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let said: Vec<String> = cx
        .update(|window, _cx| crate::a11y::tree(window))
        .into_iter()
        .filter_map(|n| n.label)
        .collect();
    let plan_row = r#"Draw the plan, Proposed, Would start on studio: os == "macos""#;
    assert!(said.iter().any(|l| l == plan_row), "{said:?}");
    assert!(
        said.iter().any(|l| l.starts_with("Orchestrator, Waits on you to start 2 tasks")),
        "{said:?}"
    );
    let project = fixtures::id("board");
    let start = |task, pin| Verb::TaskStart { project: project.clone(), task: TaskId(task), pin };

    click(cx, "project-plan-start-4");
    assert_eq!(sent(&mut queue, cx, done), [start(4, None)]);
    click(cx, "project-plan-start-all");
    assert_eq!(sent(&mut queue, cx, done), [start(4, None), start(5, None)]);

    click(cx, "project-plan-run-on-5");
    let verbs = sent(&mut queue, cx, |_| Outcome::Suggestions(Vec::new()));
    assert!(matches!(verbs.as_slice(), [Verb::PlacementSuggest { .. }]), "{verbs:?}");
    click(cx, "project-plan-picker-5-anywhere");
    assert_eq!(sent(&mut queue, cx, done), [start(5, None)], "a proposal starts where chosen");

    click(cx, "project-ask");
    let verbs = sent(&mut queue, cx, done);
    assert!(
        matches!(verbs.as_slice(), [Verb::ProjectSet { ask_to_start: Some(true), push: None, .. }]),
        "{verbs:?}"
    );
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

/// The labels the board says to assistive technology now.
fn labels(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    cx.update(|window, _cx| crate::a11y::tree(window)).into_iter().filter_map(|n| n.label).collect()
}

/// A board opens onto what changed since this client last looked. The first look has nothing
/// to compare with; hiding the board reads its timeline to the end; opening it again tells what
/// came after, what needs the person first, until they close it. Where the board holds less
/// than that, the recap reads the rest back from the server first. How far each project was
/// read is what the layout keeps across launches.
#[gpui::test]
fn a_board_opens_onto_what_changed_since_you_last_looked(cx: &mut TestAppContext) {
    use crate::project::fixtures::run;
    use crate::project::recap::Looked;

    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let show = |cx: &mut VisualTestContext, on: bool| {
        view.update_in(cx, |v, _w, cx| v.show_board(orchestrator, on, cx));
        cx.run_until_parked();
    };
    show(cx, true);
    assert!(cx.debug_bounds("project-recap").is_none(), "a first look has no recap");
    show(cx, false);
    let id = fixtures::id("board");
    let looked = view.read_with(cx, |v, _| v.projects_looked());
    assert_eq!(
        looked.iter().map(|(p, l)| (p.clone(), l.seq)).collect::<Vec<_>>(),
        [(id.clone(), 3)]
    );

    let merged = card(1, "Wire the board", TaskState::Merged, None);
    let to_merged = Moment::State { from: TaskState::Done, to: TaskState::Merged };
    let failed = card(3, "Golden files", TaskState::Waiting, Some(1));
    view.update_in(cx, |v, _w, cx| {
        v.project_update(11, task_changed("board", merged, Some(entry(4, Some(1), to_merged))), cx);
        let verified = Moment::Verified(run(false, "9c1e2f3"));
        v.project_update(12, task_changed("board", failed, Some(entry(5, Some(3), verified))), cx);
    });
    show(cx, true);
    for part in [
        "project-recap",
        "project-recap-heading",
        "project-recap-verify-failed",
        "project-recap-merged",
        "project-recap-close",
    ] {
        assert!(cx.debug_bounds(part).is_some(), "{part} is drawn");
    }
    let y =
        |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).expect("drawn").origin.y;
    assert!(
        y(cx, "project-recap-verify-failed") < y(cx, "project-recap-merged"),
        "what needs the person comes first"
    );
    let said = labels(&view, cx);
    for line in
        ["Since you last looked", "Verifier failed on #3 Golden files", "Merged #1 Wire the board"]
    {
        assert!(said.iter().any(|l| l == line), "{line} in {said:?}");
    }
    click(cx, "project-recap-close");
    assert!(cx.debug_bounds("project-recap").is_none(), "closed");
    show(cx, false);
    show(cx, true);
    assert!(cx.debug_bounds("project-recap").is_none(), "nothing new since");
    show(cx, false);

    // A launch later: the board holds only the newest entries, so the recap reads the rest.
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let term = TermRef { worker, session: orchestrator };
    let newest = vec![entry(50, Some(2), Moment::Note { text: "later".into() })];
    let tasks = vec![card(2, "Read the store", TaskState::Merged, None)];
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        let record = project("board", Some(term));
        v.projects_part(snapshot(20, vec![status(record, tasks.clone(), newest)]), cx);
        v.restore_projects_looked([(id.clone(), Looked { seq: 40, at_ms: fixtures::AT })]);
    });
    show(cx, true);
    let missed =
        vec![entry(41, Some(2), Moment::State { from: TaskState::Done, to: TaskState::Merged })];
    let mut answer = status(project("board", Some(term)), tasks, missed);
    answer.next = 51;
    let verbs = sent(&mut queue, cx, |_| Outcome::Project(Box::new(answer.clone())));
    assert_eq!(
        verbs,
        [Verb::ProjectStatus { project: id.clone(), since: Some(41), timeout_ms: 0 }],
        "read from past the last look"
    );
    let said = labels(&view, cx);
    assert!(said.iter().any(|l| l == "Merged #2 Read the store"), "{said:?}");
    assert!(
        said.iter().any(|l| l.starts_with("Since you looked, ") && l.ends_with(" ago")),
        "{said:?}"
    );
    assert!(cx.debug_bounds("project-recap-partial").is_none(), "it read all of it");

    // Further back than the server keeps: the recap says it could not read all of it.
    show(cx, false);
    view.update_in(cx, |v, _w, _cx| {
        v.restore_projects_looked([(id.clone(), Looked { seq: 30, at_ms: fixtures::AT })]);
    });
    show(cx, true);
    let verbs = sent(&mut queue, cx, |_| Outcome::Project(Box::new(answer.clone())));
    assert_eq!(verbs.len(), 1, "{verbs:?}");
    assert!(cx.debug_bounds("project-recap-partial").is_some(), "entries 31 to 40 are gone");
}

/// The board says what the project spent: its time at work in the header with the
/// orchestrator's share apart on hover, each row's time with its subtree's, and, once the
/// agents' threads hand their meters over, the cost, a context nearly full, and the plan's
/// rate windows. A thread that goes takes its meters with it.
#[gpui::test]
fn the_board_says_what_its_agents_spent(cx: &mut TestAppContext) {
    use slopty_proto::project::Spent;
    use slopty_proto::thread::{Limit, Meters};

    let (view, cx) = still_workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (_, agent) = setup.agent;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let minutes = |m: u64| Spent { active_ms: m.saturating_mul(60_000), since_ms: None };
    let mut first = on(card(1, "Wire the board", TaskState::Running, None), worker, agent);
    first.spent = minutes(12);
    let mut after = card(3, "Golden files", TaskState::Planned, Some(1));
    after.spent = minutes(30);
    view.update_in(cx, |v, _w, cx| {
        v.project_update(11, task_changed("board", first, None), cx);
        v.project_update(12, task_changed("board", after, None), cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    for part in
        ["project-spent", "project-row-project-node-1-spent", "project-row-project-node-3-spent"]
    {
        assert!(cx.debug_bounds(part).is_some(), "{part} is drawn");
    }
    for unheard in ["project-cost", "project-limit-0", "project-row-project-node-1-context"] {
        assert!(cx.debug_bounds(unheard).is_none(), "{unheard} waits for the threads");
    }
    let said = labels(&view, cx);
    assert!(said.iter().any(|l| l == "42m of work: tasks 42m, orchestrator under 1m"), "{said:?}");
    assert!(said.iter().any(|l| l.ends_with("worked 42m, 12m itself")), "{said:?}");

    let meters = Meters {
        cost_micro_usd: Some(1_250_000),
        context_tokens: Some(170_000),
        context_window: Some(200_000),
        limits: vec![Limit { name: "five-hour".to_owned(), used_bp: 8_100, resets_ms: None }],
        ..Meters::default()
    };
    view.update_in(cx, |v, _w, cx| v.thread_meters(agent, Some(meters), cx));
    cx.run_until_parked();
    for part in ["project-cost", "project-limit-0", "project-row-project-node-1-context"] {
        assert!(cx.debug_bounds(part).is_some(), "{part} is drawn");
    }
    let said = labels(&view, cx);
    assert!(
        said.iter().any(|l| l == "$1.25 spent: tasks $1.25, orchestrator not heard"),
        "{said:?}"
    );
    assert!(said.iter().any(|l| l == "5-hour 81%"), "{said:?}");
    assert!(
        said.iter().any(|l| l.ends_with("worked 42m, 12m itself, $1.25, context 85%")),
        "{said:?}"
    );

    view.update_in(cx, |v, _w, cx| v.thread_meters(agent, None, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("project-cost").is_none(), "gone with its thread");
}

/// A next step on a task's row is the person's word to its agent: "Fix CI" sends `TaskTell`
/// with what failed and what to do, never a key into its terminal, and the board says it
/// went. From the palette's line it does the same to the task the board stands on.
#[gpui::test]
fn a_next_step_is_said_to_the_task_s_agent(cx: &mut TestAppContext) {
    use crate::project::FixCi;
    use crate::project::fixtures::run;

    let (view, cx) = still_workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (_, agent) = setup.agent;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let mut failed = on(card(1, "Wire the board", TaskState::Waiting, None), worker, agent);
    failed.verified = Some(run(false, "9c1e2f3"));
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.project_update(11, task_changed("board", failed, None), cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    click(cx, "project-row-fix-ci-1");
    let verbs = sent(&mut queue, cx, done);
    let [Verb::TaskTell { project, task, text }] = verbs.as_slice() else { panic!("{verbs:?}") };
    assert_eq!((project, *task), (&fixtures::id("board"), Some(TaskId(1))));
    assert!(text.starts_with("Fix CI. `cargo gate` failed on your work at 9c1e2f3"), "{text}");
    let notice = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(notice.as_deref(), Some("Asked #1's agent to fix CI"));

    let b = board(&view, cx, orchestrator);
    b.update(cx, |b, cx| {
        b.select_by(1, cx);
        b.select_by(1, cx);
    });
    assert_eq!(b.read_with(cx, |b, _| b.picked()), Some(Some(TaskId(1))));
    cx.dispatch_action(FixCi);
    let verbs = sent(&mut queue, cx, done);
    assert!(
        matches!(verbs.as_slice(), [Verb::TaskTell { task: Some(TaskId(1)), .. }]),
        "{verbs:?}"
    );
}

/// A card whose work is on its way draws its pipeline under it, one chip a stage, and says
/// them to a screen reader: its pull request with its own checks and its open to-dos among
/// them. A card still at work draws none.
#[gpui::test]
fn a_card_draws_its_pipeline_once_its_work_is_on_its_way(cx: &mut TestAppContext) {
    use slopty_proto::agent::PullRequest;
    use slopty_proto::project::{Checks, ChecksState, NativeCounts};

    use crate::project::fixtures::queued;

    let (view, cx) = still_workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (_, agent) = setup.agent;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let mut done =
        on(queued(card(1, "Wire the board", TaskState::Done, None), 10, "1111111"), worker, agent);
    done.pr = Some(PullRequest {
        number: 42,
        url: "https://github.com/o/r/pull/42".into(),
        review: None,
        merge_request: false,
    });
    done.checks = Some(Checks {
        state: ChecksState::Pending,
        passed: 4,
        failed: 0,
        pending: 2,
        skipped: 0,
        failing: Vec::new(),
        why: None,
        at_ms: fixtures::AT,
    });
    done.natives = NativeCounts { agents: 0, running: 0, todos: 1, done: 0 };
    let working = card(2, "Golden files", TaskState::Running, None);
    view.update_in(cx, |v, _w, cx| {
        v.project_update(11, task_changed("board", done, None), cx);
        v.project_update(12, task_changed("board", working, None), cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    let b = board(&view, cx, orchestrator);
    b.update(cx, |b, cx| b.show(Lens::Board, cx));
    cx.run_until_parked();
    for part in [
        "project-card-1-branch",
        "project-card-1-pull",
        "project-card-1-checks",
        "project-card-1-todos",
    ] {
        assert!(cx.debug_bounds(part).is_some(), "{part} is drawn");
    }
    assert!(cx.debug_bounds("project-card-2-branch").is_none(), "nothing on its way yet");
    let said = labels(&view, cx);
    assert!(said.iter().any(|l| l == "2 of 6 checks running"), "{said:?}");
    assert!(said.iter().any(|l| l == "1 to-do open"), "{said:?}");
}

/// The board's foot is a line to the orchestrator: `c` puts the keyboard on it, a letter typed
/// there is a letter and not the board's key, and Enter sends the person's words as
/// `TaskTell` with no task, clearing the line. Words the server refuses come back on it, and
/// Escape gives the board its keys back.
#[gpui::test]
fn the_board_talks_to_its_orchestrator(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (_, agent) = setup.agent;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let finished = on(card(1, "Wire the board", TaskState::Done, None), worker, agent);
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.project_update(11, task_changed("board", finished, None), cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("project-composer").is_some(), "the line is drawn");
    let b = board(&view, cx, orchestrator);
    let line = |cx: &mut VisualTestContext| b.read_with(cx, ProjectView::composing);

    cx.simulate_keystrokes("c");
    cx.simulate_input("merge #1 after the golden lands");
    assert_eq!(line(cx).as_deref(), Some("merge #1 after the golden lands"));
    assert!(sent(&mut queue, cx, done).is_empty(), "no letter was the board's key");
    cx.simulate_keystrokes("enter");
    let verbs = sent(&mut queue, cx, done);
    let [Verb::TaskTell { project, task: None, text }] = verbs.as_slice() else {
        panic!("{verbs:?}")
    };
    assert_eq!(
        (project, text.as_str()),
        (&fixtures::id("board"), "merge #1 after the golden lands")
    );
    assert_eq!(line(cx).as_deref(), Some(""), "the line is cleared");

    cx.simulate_input("Split it in two");
    cx.simulate_keystrokes("enter");
    let refused = |_: &Verb| Outcome::Error {
        code: slopty_proto::orchestration::ErrorCode::Invalid,
        message: "board has no orchestrator running to hear it".to_owned(),
    };
    assert_eq!(sent(&mut queue, cx, refused).len(), 1);
    assert_eq!(line(cx).as_deref(), Some("Split it in two"), "refused words come back");
    let notice = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(notice.as_deref(), Some("board has no orchestrator running to hear it"));

    cx.simulate_keystrokes("escape 3");
    let lens = b.read_with(cx, |b, _| b.lens());
    assert_eq!(lens, Lens::Timeline, "the board has its keys back");
}

/// Every node says where it is at a glance: the worker and its system on its row and its card,
/// the card with why it went there. A task's place still to come moves from it ("Run on…"
/// asks the server to rank the workers); a running one's shows the machines lens.
#[gpui::test]
fn every_node_says_where_it_runs_and_a_waiting_one_moves_from_there(cx: &mut TestAppContext) {
    use slopty_proto::project::Placed;
    let (view, cx) = still_workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (_, agent) = setup.agent;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let mut running = on(card(1, "Wire the board", TaskState::Running, None), worker, agent);
    if let Some(a) = running.assignment.as_mut() {
        a.placed = Some(Placed { pinned: false, score: 0, why: "Apple work".into() });
    }
    let mut pinned = card(3, "Golden files", TaskState::Planned, Some(1));
    pinned.pin = Some(worker);
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.project_update(11, task_changed("board", running, None), cx);
        v.project_update(12, task_changed("board", pinned, None), cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    for node in ["orchestrator", "1", "2", "3"] {
        let id = format!("project-row-project-node-{node}-where");
        assert!(cx.debug_bounds(Box::leak(id.clone().into_boxed_str())).is_some(), "{id}");
    }
    let said = labels(&view, cx);
    let row = said.iter().find(|l| l.starts_with("Wire the board")).expect("task 1's row");
    assert!(row.contains("studio \u{b7} macOS"), "{row}");
    assert!(
        said.iter().any(|l| l.starts_with("Runs on studio, macOS. Branch slopty/board/1. Why: Apple work")),
        "{said:?}"
    );
    assert!(said.iter().any(|l| l.starts_with("Pinned to studio, macOS")), "{said:?}");

    click(cx, "project-row-project-node-3-where");
    let verbs = sent(&mut queue, cx, |_| Outcome::Suggestions(Vec::new()));
    assert!(
        matches!(verbs.as_slice(), [Verb::PlacementSuggest { task: Some(TaskId(3)), .. }]),
        "a place still to come moves: {verbs:?}"
    );
    let b = board(&view, cx, orchestrator);
    b.update(cx, |b, cx| b.show(Lens::Board, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("project-card-project-node-1-where").is_some(), "on its card");
    assert!(cx.debug_bounds("project-card-1-why").is_some(), "and why it went there");
    b.update(cx, |b, cx| b.show(Lens::Tree, cx));
    cx.run_until_parked();
    let _answered = sent(&mut queue, cx, done);
    click(cx, "project-row-project-node-1-where");
    assert_eq!(b.read_with(cx, |b, _| b.lens()), Lens::Machines, "a running one's machines");

    // The machines lens says what the project's work needs of them.
    let mut needing = project("board", Some(TermRef { worker, session: orchestrator }));
    needing.needs = vec![slopty_proto::project::Need {
        name: "Apple work".into(),
        paths: vec!["apps/slopty".into()],
        require: vec![r#"os == "macos""#.into()],
        prefer: Vec::new(),
    }];
    let update = ProjectUpdate {
        project: fixtures::id("board"),
        record: Some(needing),
        task: None,
        native: None,
        entry: None,
    };
    view.update_in(cx, |v, _w, cx| v.project_update(13, update, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("project-machines-needs").is_some(), "its heading");
    let said = labels(&view, cx);
    let need = r#"Apple work, apps/slopty, Requires os == "macos""#;
    assert!(said.iter().any(|l| l == need), "{said:?}");
}

/// Claude Code's subagent `id` of type Explore, running under task 1.
fn native(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, seq: u64, id: &str) {
    use slopty_proto::project::{Native, NativeAgent, NativeChange};
    let agent = Native::Agent(NativeAgent {
        id: id.to_owned(),
        kind: "Explore".to_owned(),
        started_ms: WallMs::ZERO,
        stopped_ms: None,
        transcript: None,
        last: None,
    });
    let update = ProjectUpdate {
        project: fixtures::id("board"),
        record: None,
        task: None,
        native: Some(NativeChange { task: Some(TaskId(1)), native: agent }),
        entry: None,
    };
    view.update_in(cx, |v, _w, cx| v.project_update(seq, update, cx));
    cx.run_until_parked();
}

/// A click on one of Claude Code's own subagents in the tree opens it as its task's row opens
/// the task: the agent's tile, showing its face, on the subagent's thread. Where the tile shows
/// the thread view that is its way into a subagent (the bar that leads back); before the
/// session's thread is known, the conversation face's.
#[gpui::test]
fn a_click_on_a_subagent_opens_its_thread(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::TableFrame;
    use slopty_proto::thread::{Cursor, Link};

    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (agent_tile, agent) = setup.agent;
    native(&view, cx, 11, "a1");
    view.update_in(cx, |v, _w, cx| v.show_board(orchestrator, true, cx));
    cx.run_until_parked();

    let row = cx.debug_bounds("project-node-1-native-a1").expect("the subagent's row").center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(agent_tile), "its agent's tile");
    let face = view.read_with(cx, |v, _| v.face_shown(agent));
    assert!(face, "showing its face");
    let thread =
        view.read_with(cx, |v, cx| v.conversation(agent).map(|f| f.read(cx).thread().clone()));
    assert_eq!(
        thread,
        Some(slopty_proto::conversation::ThreadId::Agent("a1".to_owned())),
        "on the subagent's thread"
    );

    // The worker's table names the session's thread and the subagent's under it.
    let key = setup.fake.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(agent);
    let root = state.row(WallMs::ZERO);
    let mut sub = root.clone();
    sub.id = root.id.subagent("a1");
    sub.terminal = None;
    sub.parent = Some(Link { thread: root.id, item: slopty_proto::thread::ItemId("call".into()) });
    let table =
        TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows: vec![root, sub.clone()] };
    view.update_in(cx, |v, _w, cx| {
        v.thread_table(key, &table, cx);
        v.focus_tile(setup.orchestrator.0, cx);
    });
    cx.run_until_parked();
    let row = cx.debug_bounds("project-node-1-native-a1").expect("the subagent's row").center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(agent_tile));
    let shown = view.read_with(cx, |v, cx| v.thread_face(agent).map(|t| t.read(cx).shown()));
    assert_eq!(shown, Some(sub.id), "the thread view on the subagent's thread");
}

/// The board sets how its project's work is checked: the header's toggle opens a panel holding
/// the verifier and the reviewer as the project has them, the keyboard in the verifier; ↩
/// sends both as the person's word and closes it, a reviewer switched on with no brief getting
/// the default one. Esc closes it with nothing sent, and a reviewer switched off is sent as
/// none.
#[gpui::test]
fn a_board_sets_its_verifier_and_review(cx: &mut TestAppContext) {
    use slopty_proto::project::LimitsChange;

    use crate::project::BRIEF;
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    let project = fixtures::id("board");
    let set = |verifier: &str, review: &str| Verb::ProjectSet {
        project: project.clone(),
        orchestrator: None,
        verifier: Some(verifier.to_owned()),
        review: Some(review.to_owned()),
        push: None,
        ask_to_start: None,
        limits: LimitsChange::default(),
        metadata: None,
        members: None,
    };
    assert!(cx.debug_bounds("project-checks-panel").is_none(), "closed until asked");

    click(cx, "project-checks");
    assert!(cx.debug_bounds("project-checks-panel").is_some(), "the panel opens");
    let verifier = board(&view, cx, orchestrator).read_with(cx, ProjectView::checks_typed);
    assert_eq!(verifier, Some(("cargo gate".to_owned(), None)), "as the project has it");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("cargo test --workspace");
    click(cx, "project-review-switch");
    cx.simulate_keystrokes("enter");
    assert_eq!(sent(&mut queue, cx, done), [set("cargo test --workspace", BRIEF)]);
    assert!(cx.debug_bounds("project-checks-panel").is_none(), "saved, it closes");

    click(cx, "project-checks");
    cx.simulate_keystrokes("escape");
    assert!(cx.debug_bounds("project-checks-panel").is_none(), "Esc closes it");
    assert_eq!(sent(&mut queue, cx, done), [], "with nothing sent");

    click(cx, "project-checks");
    click(cx, "project-checks-save");
    assert_eq!(sent(&mut queue, cx, done), [set("cargo gate", "")], "no reviewer is none");
}

/// A moment that holds a task up is said as it lands: a notice with the app in front, unless
/// the project's board has the person's eye, and a note leading to the orchestrator while the
/// app is away. A moment the board alone shows says nothing.
#[gpui::test]
fn a_project_s_failure_is_said_as_it_lands(cx: &mut TestAppContext) {
    use crate::project::fixtures::run;
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (orchestrator_tile, orchestrator) = setup.orchestrator;
    let (agent_tile, _) = setup.agent;
    let failed = |seq: u64, passed: bool| {
        let entry = entry(seq, Some(1), Moment::Verified(run(passed, "abc1234")));
        task_changed("board", card(1, "Wire the board", TaskState::Running, None), Some(entry))
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

    view.update_in(cx, |v, _w, cx| {
        v.focus_tile(agent_tile, cx);
        v.project_update(20, failed(4, true), cx);
    });
    cx.run_until_parked();
    assert_eq!(toast(cx), None, "a pass is the board's to show");
    view.update_in(cx, |v, _w, cx| v.project_update(21, failed(5, false), cx));
    cx.run_until_parked();
    assert_eq!(
        toast(cx).as_deref(),
        Some("Ship the project board: #1 Wire the board: its verifier failed")
    );

    let said = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.toast_texts().len());
    let before = said(cx);
    view.update_in(cx, |v, _w, cx| {
        v.focus_tile(orchestrator_tile, cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| v.project_update(22, failed(6, false), cx));
    cx.run_until_parked();
    assert_eq!(said(cx), before, "the board in front says it already");

    view.update_in(cx, |v, _w, cx| {
        v.set_app_active(false, cx);
        v.project_update(23, failed(7, false), cx);
    });
    cx.run_until_parked();
    assert!(events.borrow().contains(&WorkspaceEvent::ProjectNews), "{:?}", events.borrow());
    let news = view.update(cx, |v, _| v.take_project_news());
    let [note] = news.as_slice() else { panic!("one note: {news:?}") };
    assert_eq!(note.route.about, attention::About::Session(orchestrator), "to the orchestrator");
    assert_eq!((note.title.as_str(), note.project.as_str()), ("Ship the project board", "board"));
    assert_eq!(note.body, "#1 Wire the board: its verifier failed");
    assert_eq!(view.update(cx, |v, _| v.take_project_news()), [], "taken once");
}

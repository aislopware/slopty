//! A project's board in its orchestrator's tile: turned to and from with ⇧⌘J, kept in step
//! with the server's changes, and every row a way to its agent's tile.

use slopty_core::WorkerId;
use slopty_proto::orchestration::{Outcome, TermRef, Verb};
use slopty_proto::project::{Moment, ProjectUpdate, TaskId, TaskState};

use super::*;
use crate::project::ProjectView;
use crate::project::fixtures::{self, card, entry, on, project, snapshot, status, task_changed};

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
    let mut after = card(3, "Golden files", TaskState::Planned);
    after.depends_on = vec![TaskId(1)];
    let tasks = vec![
        on(card(1, "Wire the board", TaskState::Running), worker, agent),
        on(card(2, "Read the store", TaskState::Blocked), worker, blocked_session),
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

/// ⇧⌘J turns the orchestrator's tile to its board, which takes the keyboard: the header and
/// the lanes, what waits on the person first. ↓ and ↩ open a task's agent in its own tile; back
/// on the orchestrator the board has the keyboard again, and ⇧⌘J gives the terminal back.
#[gpui::test]
fn the_orchestrators_tile_turns_to_its_board_and_opens_its_agents(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (orchestrator_tile, orchestrator) = setup.orchestrator;
    let (agent_tile, agent) = setup.agent;
    // The orchestrator on its TUI, where ⇧⌘J gives the keyboard back to.
    view.update_in(cx, |v, _w, cx| v.show_face(orchestrator, false, cx));
    cx.run_until_parked();
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
        "project-lane-needs-you",
        "project-lane-working",
        "project-lane-up-next",
        "project-card-1",
        "project-card-2",
        "project-card-3",
    ] {
        assert!(bounds(cx, part), "{part} is drawn");
    }
    assert!(!bounds(cx, "project-needs-you"), "#2 says it in its lane; the orchestrator works");
    let tile_bounds = view.read_with(cx, |v, _| v.tile_bounds(orchestrator_tile)).expect("drawn");
    let card = cx.debug_bounds("project-card-3").expect("drawn");
    assert!(tile_bounds.contains(&card.origin), "in the orchestrator's tile");
    let b = board(&view, cx, orchestrator);
    let board_focused = |cx: &mut VisualTestContext, b: &Entity<ProjectView>| {
        cx.update(|window, cx| b.read(cx).focus_handle(cx).is_focused(window))
    };
    assert!(board_focused(cx, &b), "the board takes the keyboard");
    assert_eq!(focused(&view, cx), Some(orchestrator_tile));

    // The lanes run Needs you (#2), Working (#1), Up next (#3).
    cx.simulate_keystrokes("down down enter");
    cx.run_until_parked();
    assert_eq!(b.read_with(cx, |b, _| b.picked()), Some(Some(TaskId(1))));
    assert_eq!(focused(&view, cx), Some(agent_tile), "↩ went to task 1's agent");
    assert!(view.read_with(cx, |v, _| v.face_shown(agent)), "on the agent's thread");

    view.update_in(cx, |v, _w, cx| v.focus_tile(orchestrator_tile, cx));
    cx.run_until_parked();
    assert!(board_focused(cx, &b), "back on the orchestrator, the board has the keyboard");
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(b.read_with(cx, |b, _| b.picked()), Some(Some(TaskId(3))), "the board's keys");

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
    let mut tasks: Vec<_> = (1..=24).map(|n| card(n, "Planned work", TaskState::Planned)).collect();
    tasks.push(card(25, "Ship it", TaskState::Done));
    let term = TermRef { worker, session: orchestrator };
    view.update_in(cx, |v, _w, cx| {
        v.projects_part(
            snapshot(10, vec![status(project("board", Some(term)), tasks, vec![])]),
            cx,
        );
        v.show_board(orchestrator, true, cx);
    });
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

/// A click on a card opens its agent; a card whose agent has no tile here says so rather than
/// going nowhere.
#[gpui::test]
fn a_click_opens_a_nodes_agent_or_says_why_not(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (agent_tile, _) = setup.agent;
    view.update_in(cx, |v, _w, cx| v.show_board(orchestrator, true, cx));
    cx.run_until_parked();

    let row = cx.debug_bounds("project-card-1").expect("drawn").center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(agent_tile));

    view.update_in(cx, |v, _w, cx| v.focus_tile(setup.orchestrator.0, cx));
    cx.run_until_parked();
    let row = cx.debug_bounds("project-card-2").expect("drawn").center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    let notice = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(
        notice.as_deref(),
        Some("#2's agent has no tile here"),
        "no tile for {:?}",
        setup.blocked
    );

    let row = cx.debug_bounds("project-card-3").expect("drawn").center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()).as_deref(), Some("#3 has no agent yet"));
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
        on(card(1, "Wire the board", TaskState::Merged), worker, agent),
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
        on(card(1, "Wire the board", TaskState::Blocked), worker, agent),
        Some(entry(4, Some(1), Moment::State { from: TaskState::Running, to: TaskState::Blocked })),
    );
    view.update_in(cx, |v, _w, cx| v.project_update(11, now_blocked, cx));
    fresh(cx, "task 1 waits on the person");
    let lane = cx.debug_bounds("project-lane-needs-you").expect("drawn");
    let card = cx.debug_bounds("project-card-1").expect("drawn");
    assert!(lane.contains(&card.center()), "it joins what needs you");
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

/// The orchestrator waiting on the person stands over the lanes, inside the column the lanes
/// stand in, never over the tile's edge; a task waiting on them says it in its own lane, not
/// in the band too. The lanes share the tile's width equally, however many there are.
#[gpui::test]
fn the_board_says_each_thing_once_and_fills_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (orchestrator_tile, orchestrator) = setup.orchestrator;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(
            AgentEvent { detail: Some("Merge #1 now?".into()), ..blocked(orchestrator) },
            cx,
        );
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    let band = cx.debug_bounds("project-needs-you").expect("the orchestrator waits on you");
    let tile = view.read_with(cx, |v, _| v.tile_bounds(orchestrator_tile)).expect("drawn");
    let lanes: Vec<Bounds<Pixels>> = ["needs-you", "working", "up-next"]
        .map(|lane| {
            let id: &'static str = Box::leak(format!("project-lane-{lane}").into_boxed_str());
            cx.debug_bounds(id).expect("drawn")
        })
        .into();
    assert!(band.left() > tile.left() + px(0.5), "inside the tile: {band:?} in {tile:?}");
    assert!(
        (band.left() - lanes[0].left()).abs() < px(0.5),
        "on the lanes' left edge: {band:?}, {:?}",
        lanes[0]
    );
    assert!(cx.debug_bounds("project-needs-orchestrator").is_some());
    assert!(
        cx.debug_bounds("project-needs-project-node-2").is_none(),
        "#2 says it in its lane, not in the band too"
    );

    let body = cx.debug_bounds("project-body").expect("drawn");
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
}

/// A verifier shows on its task's card: a run under way, or its verdict and the commits judged,
/// a failure with its last lines. Ready to merge runs in the queue's order. "Output" opens the
/// verifier's terminal without opening the agent. A terminal that has closed says so.
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

    let mut failed = card(4, "Read the snapshot", TaskState::Waiting);
    failed.verified = Some(run(false, "4a7aa6d0"));
    let why = StepState::Failed { why: "exit 101".into() };
    failed.step =
        Some(step(StepKind::Verify, worker, why, Some(TermRef { worker, session: kept })));
    let mut running = card(5, "Draw the lanes", TaskState::Verifying);
    let line = StepState::Running { phase: "Compiling slopty-ui".into(), percent: None };
    running.step =
        Some(step(StepKind::Verify, worker, line, Some(TermRef { worker, session: closed })));
    let later = queued(card(6, "Hold it to goldens", TaskState::Done), 20, "6666666");
    let sooner = queued(card(7, "Write the decision", TaskState::Done), 10, "7777777");
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
        "project-card-4-check",
        "project-card-4-tail",
        "project-card-4-output",
        "project-card-5-check",
        "project-card-6-check",
        "project-card-7-check",
    ] {
        assert!(drawn(cx, part), "{part} is drawn");
    }
    assert!(!drawn(cx, "project-card-5-tail"), "a run under way has no last lines");

    click(cx, "project-card-4-output");
    assert_eq!(focused(&view, cx), Some(kept_tile), "the verifier's terminal, not the agent's");
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()), None, "the card's own click held back");

    view.update_in(cx, |v, _w, cx| v.focus_tile(orchestrator_tile, cx));
    cx.run_until_parked();
    click(cx, "project-card-5-output");
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("The verifier's terminal has closed")
    );
    assert_eq!(focused(&view, cx), Some(orchestrator_tile), "and nothing else opened");
    let y =
        |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).expect("drawn").origin.y;
    assert!(
        y(cx, "project-card-7") < y(cx, "project-card-6"),
        "the one waiting longest is next to merge"
    );
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
    let at = reveal(cx, selector);
    cx.simulate_click(at.center(), Modifiers::none());
    cx.run_until_parked();
}

/// Scroll the board's body until what `selector` names stands inside it, as a person scrolls to
/// a card below the fold before clicking it, and say where it is then. What the body does not
/// hold (the header, the message) does not move with it, and is clicked where it stands.
fn reveal(cx: &mut VisualTestContext, selector: &str) -> Bounds<Pixels> {
    let name: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    let at = cx.debug_bounds(name).expect(selector);
    let Some(body) = cx.debug_bounds("project-body") else { return at };
    let by = if at.bottom() > body.bottom() {
        body.bottom() - at.bottom() - px(8.0)
    } else if at.top() < body.top() {
        body.top() - at.top() + px(8.0)
    } else {
        return at;
    };
    let scroll = |cx: &mut VisualTestContext, by: Pixels| {
        cx.simulate_event(ScrollWheelEvent {
            position: body.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), by)),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
            momentum_phase: None,
        });
        cx.run_until_parked();
    };
    scroll(cx, by);
    let now = cx.debug_bounds(name).expect(selector);
    if now == at {
        // Not the body's: put back whatever the wheel moved.
        scroll(cx, -by);
        return cx.debug_bounds(name).expect(selector);
    }
    now
}

/// The board's actions reach the server as the person's word. A finished task's Merge and a
/// merged task's Push again are buttons on its row and its card, which send `TaskMerge` and
/// `TaskPush` without opening the agent; a refusal is said in the server's words. The header's push
/// toggle sets the project's pushing, and its terminal button turns the tile back to the
/// orchestrator. "Delete the project" asks twice.
#[gpui::test]
fn the_boards_actions_reach_the_server(cx: &mut TestAppContext) {
    use slopty_proto::orchestration::ErrorCode;

    use crate::project::{DeleteProject, MergeTask};
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.project_update(11, task_changed("board", card(5, "Land it", TaskState::Done), None), cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    let project = fixtures::id("board");

    click(cx, "project-card-merge-5");
    assert_eq!(
        sent(&mut queue, cx, done),
        [Verb::TaskMerge { project: project.clone(), task: TaskId(5) }]
    );
    assert!(shown(&view, cx, orchestrator), "the button held the card's own click back");
    assert!(
        cx.debug_bounds("project-card-merge-1").is_none(),
        "a task at work has nothing to merge"
    );

    let mut unpushed = card(6, "Pushed late", TaskState::Merged);
    unpushed.merge = Some(slopty_proto::project::Merge::Merged {
        target: "main".into(),
        head: "abcdef0123".into(),
        at_ms: fixtures::AT,
        pushed: false,
        push_failed: Some("could not read Username".into()),
    });
    view.update_in(cx, |v, _w, cx| v.project_update(13, task_changed("board", unpushed, None), cx));
    cx.run_until_parked();
    click(cx, "project-card-push-again-6");
    let refused = |_: &Verb| Outcome::Error {
        code: ErrorCode::Conflict,
        message: "#6 changed under the board".into(),
    };
    assert_eq!(
        sent(&mut queue, cx, refused),
        [Verb::TaskPush { project: project.clone(), task: TaskId(6) }]
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("#6 changed under the board"),
        "a refusal in the server's words"
    );

    let b = board(&view, cx, orchestrator);

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
    cx.dispatch_action(MergeTask);
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("#1 has nothing to merge")
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
    assert!(cx.debug_bounds("project-card-stop-1").is_none(), "only on the task stood on");
    let b = board(&view, cx, orchestrator);
    b.update(cx, |b, cx| {
        b.select_by(1, cx);
        b.select_by(1, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("project-card-cancel-1").is_some());
    assert!(cx.debug_bounds("project-card-stop-3").is_none(), "task 3 is not stood on");
    click(cx, "project-card-stop-1");
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

/// A task not started yet offers "Run on…" once it is stood on, never on every card at once.
/// It opens the workers under the task, each with its system and its agents and one away said
/// so; a choice pins the task there and closes the picker.
#[gpui::test]
fn a_task_not_started_is_pinned_from_its_card(cx: &mut TestAppContext) {
    use slopty_proto::project::RunOn;
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
    let y = |cx: &mut VisualTestContext, s: String| {
        cx.debug_bounds(Box::leak(s.into_boxed_str())).map(|b| b.origin.y)
    };
    assert!(y(cx, "project-card-run-on-3".to_owned()).is_none(), "not on a card at rest");
    let b = board(&view, cx, orchestrator);
    // The lanes run Needs you (#2), Working (#1), Up next (#3).
    b.update(cx, |b, cx| {
        for _ in 0..3 {
            b.select_by(1, cx);
        }
    });
    cx.run_until_parked();
    assert_eq!(b.read_with(cx, |b, _| b.picked()), Some(Some(TaskId(3))));
    assert!(y(cx, "project-card-run-on-1".to_owned()).is_none(), "#1 has started");

    let away = WorkerId::new();
    click(cx, "project-card-run-on-3");
    let verbs = sent(&mut queue, cx, |_| {
        Outcome::Facts(vec![facts(worker, "studio", true), facts(away, "attic", false)])
    });
    assert_eq!(verbs, [Verb::WorkerFacts { worker: None }]);
    for option in ["anywhere", "0", "1"] {
        let id = format!("project-card-picker-3-{option}");
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
    assert!(said.iter().any(|l| l == "studio, macos \u{b7} 3 agents"), "{said:?}");
    assert!(said.iter().any(|l| l == "attic, macos \u{b7} 3 agents \u{b7} offline"), "{said:?}");

    click(cx, "project-card-picker-3-0");
    let verbs = sent(&mut queue, cx, |_| Outcome::Done);
    let [Verb::TaskUpdate { task: TaskId(3), change, .. }] = verbs.as_slice() else {
        panic!("a pin: {verbs:?}");
    };
    assert_eq!(change.run_on, Some(RunOn::Worker(worker)));
    assert!(y(cx, "project-card-picker-3".to_owned()).is_none(), "the choice closes it");
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

    let merged = card(1, "Wire the board", TaskState::Merged);
    let to_merged = Moment::State { from: TaskState::Done, to: TaskState::Merged };
    let failed = card(3, "Golden files", TaskState::Waiting);
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
    let tasks = vec![card(2, "Read the store", TaskState::Merged)];
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

/// The board says what the project spent: its time at work in the header, with the
/// orchestrator's share apart on hover. No context meter, plan window or dollar figure: the
/// status bar has the plan's windows.
#[gpui::test]
fn the_board_says_what_its_agents_spent(cx: &mut TestAppContext) {
    use slopty_proto::project::Spent;

    let (view, cx) = still_workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (_, agent) = setup.agent;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let minutes = |m: u64| Spent { active_ms: m.saturating_mul(60_000), since_ms: None };
    let mut first = on(card(1, "Wire the board", TaskState::Running), worker, agent);
    first.spent = minutes(12);
    let mut after = card(3, "Golden files", TaskState::Planned);
    after.spent = minutes(30);
    view.update_in(cx, |v, _w, cx| {
        v.project_update(11, task_changed("board", first, None), cx);
        v.project_update(12, task_changed("board", after, None), cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("project-spent").is_some(), "the header says it");
    for gone in ["project-limit-0", "project-cost", "project-card-1-context"] {
        assert!(cx.debug_bounds(gone).is_none(), "{gone} is not the board's");
    }
    let said = labels(&view, cx);
    assert!(said.iter().any(|l| l == "42m of work: tasks 42m, orchestrator under 1m"), "{said:?}");
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
    let mut failed = on(card(1, "Wire the board", TaskState::Waiting), worker, agent);
    failed.verified = Some(run(false, "9c1e2f3"));
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.project_update(11, task_changed("board", failed, None), cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    click(cx, "project-card-fix-ci-1");
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
        on(queued(card(1, "Wire the board", TaskState::Done), 10, "1111111"), worker, agent);
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
    let working = card(2, "Golden files", TaskState::Running);
    view.update_in(cx, |v, _w, cx| {
        v.project_update(11, task_changed("board", done, None), cx);
        v.project_update(12, task_changed("board", working, None), cx);
        v.show_board(orchestrator, true, cx);
    });
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
    let finished = on(card(1, "Wire the board", TaskState::Done), worker, agent);
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

    cx.simulate_keystrokes("escape down");
    let picked = b.read_with(cx, |b, _| b.picked());
    assert_eq!(picked, Some(Some(TaskId(2))), "the board has its keys back");
}

/// Every card says where it is at a glance: the worker and its system, a pinned one's card
/// saying why it is there. A task's place still to come moves from it ("Run on…" asks the
/// server for the workers); a running one's is only words.
#[gpui::test]
fn every_card_says_where_it_runs_and_a_waiting_one_moves_from_there(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (_, agent) = setup.agent;
    let worker = fixtures_worker(&view, cx, orchestrator);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let running = on(card(1, "Wire the board", TaskState::Running), worker, agent);
    let mut pinned = card(3, "Golden files", TaskState::Planned);
    pinned.pin = Some(worker);
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.project_update(11, task_changed("board", running, None), cx);
        v.project_update(12, task_changed("board", pinned, None), cx);
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    for task in ["1", "3"] {
        let id = format!("project-card-project-node-{task}-where");
        assert!(cx.debug_bounds(Box::leak(id.clone().into_boxed_str())).is_some(), "{id}");
    }
    assert!(cx.debug_bounds("project-card-3-why").is_some(), "a pin says why it is there");
    let said = labels(&view, cx);
    let card1 = said.iter().find(|l| l.starts_with("Wire the board")).expect("task 1's card");
    assert!(card1.contains("studio \u{b7} macOS"), "{card1}");
    assert!(
        said.iter().any(|l| l.starts_with("Runs on studio, macOS. Branch slopty/board/1")),
        "{said:?}"
    );
    assert!(said.iter().any(|l| l.starts_with("Pinned to studio, macOS")), "{said:?}");

    click(cx, "project-card-project-node-3-where");
    let verbs = sent(&mut queue, cx, |_| Outcome::Facts(Vec::new()));
    assert_eq!(verbs, [Verb::WorkerFacts { worker: None }], "a place still to come moves");
    // Last: the click falls through to its card, which opens its agent and moves the strip.
    click(cx, "project-card-project-node-1-where");
    assert_eq!(sent(&mut queue, cx, done), [], "a running one's place stays");
}

/// The board sets how its project's work is checked: the header's toggle opens a panel holding
/// the verifier as the project has it, the keyboard in it; ↩ sends it as the person's word and
/// closes it. Esc closes it with nothing sent, and Save sends what the field holds.
#[gpui::test]
fn a_board_sets_its_verifier(cx: &mut TestAppContext) {
    use slopty_proto::project::LimitsChange;

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
    let set = |verifier: &str| Verb::ProjectSet {
        project: project.clone(),
        orchestrator: None,
        verifier: Some(verifier.to_owned()),
        push: None,
        limits: LimitsChange::default(),
        metadata: None,
        members: None,
    };
    assert!(cx.debug_bounds("project-checks-panel").is_none(), "closed until asked");

    click(cx, "project-checks");
    assert!(cx.debug_bounds("project-checks-panel").is_some(), "the panel opens");
    let verifier = board(&view, cx, orchestrator).read_with(cx, ProjectView::checks_typed);
    assert_eq!(verifier.as_deref(), Some("cargo gate"), "as the project has it");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("cargo test --workspace");
    cx.simulate_keystrokes("enter");
    assert_eq!(sent(&mut queue, cx, done), [set("cargo test --workspace")]);
    assert!(cx.debug_bounds("project-checks-panel").is_none(), "saved, it closes");

    click(cx, "project-checks");
    cx.simulate_keystrokes("escape");
    assert!(cx.debug_bounds("project-checks-panel").is_none(), "Esc closes it");
    assert_eq!(sent(&mut queue, cx, done), [], "with nothing sent");

    click(cx, "project-checks");
    click(cx, "project-checks-save");
    assert_eq!(sent(&mut queue, cx, done), [set("cargo gate")], "what the field holds");
}

/// The server's notice of a project's held-up work leads to its orchestrator's tile, and is a
/// note of its own per timeline entry, stacked under its project; a project with no orchestrator
/// has nowhere to lead.
#[gpui::test]
fn a_project_s_notice_leads_to_its_orchestrator_stacked_by_project(cx: &mut TestAppContext) {
    use slopty_platform::notify::Memory;
    use slopty_proto::thread::attention::{Notice, NoticeKind, Subject};

    use crate::workspace::attention::{About, Attention};
    let (view, cx) = workspace(cx);
    let setup = setup(&view, cx);
    let (orchestrator_tile, orchestrator) = setup.orchestrator;
    let board = slopty_proto::project::ProjectId::new("board").unwrap();
    let mut notice = Notice {
        kind: NoticeKind::Project,
        about: Subject::Project { project: board, entry: 7 },
        tile: Some(TermRef { worker: WorkerId::new(), session: orchestrator }),
        title: "Ship the project board".into(),
        text: "#1 Wire the board: its verifier failed".into(),
        worked_ms: None,
        via: None,
    };
    let heard = view.read_with(cx, |v, _| v.heard(&notice)).expect("a note");
    assert_eq!(heard.route.about, About::Session(orchestrator), "to the orchestrator");
    assert_eq!(heard.route.item, Some(orchestrator_tile.item));
    assert_eq!(
        (heard.title.as_str(), heard.body.as_str()),
        (notice.title.as_str(), notice.text.as_str())
    );
    let memory = Rc::new(Memory::default());
    let mut attention = Attention::new(Rc::<Memory>::clone(&memory));
    attention.set_active(false);
    attention.notice(&heard);
    let posted = memory.posted();
    let [note] = posted.as_slice() else { panic!("one note: {posted:?}") };
    assert_eq!(note.id, "project-board-7", "one per timeline entry");
    assert_eq!(note.thread.as_deref(), Some("board"), "stacked under its project");

    notice.tile = None;
    assert!(view.read_with(cx, |v, _| v.heard(&notice)).is_none(), "nowhere to lead");
}

/// The bar under the header fills only with what merged: empty while nothing has, though every
/// task stands in a lane, then the merged tasks' share of them all, its value said as a
/// percentage. How the rest stands is the lanes' counts, not the bar's.
#[gpui::test]
fn the_bar_fills_only_with_what_merged(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    view.update_in(cx, |v, _w, cx| v.show_board(orchestrator, true, cx));
    cx.run_until_parked();
    let track = cx.debug_bounds("project-bar").expect("drawn");
    let fill = |cx: &mut VisualTestContext| {
        cx.debug_bounds("project-bar-share-fill").map_or(px(0.0), |b| b.size.width)
    };
    assert!(fill(cx) < px(0.5), "nothing merged: an empty track, {:?}", fill(cx));

    let worker = fixtures_worker(&view, cx, orchestrator);
    let (_, agent) = setup.agent;
    let merged = on(card(1, "Wire the board", TaskState::Merged), worker, agent);
    view.update_in(cx, |v, _w, cx| v.project_update(11, task_changed("board", merged, None), cx));
    cx.run_until_parked();
    // The fill glides to its share; past the glide it stands at a third of the track.
    cx.executor().advance_clock(Duration::from_secs(1));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let third = track.size.width / 3.0;
    assert!((fill(cx) - third).abs() < px(1.0), "one of three merged: {:?} of {track:?}", fill(cx));
    let said = labels(&view, cx);
    assert!(said.iter().any(|l| l == "1 of 3 merged"), "{said:?}");
}

/// The message to the orchestrator has its own send control: a click sends what is written, as
/// ↵ does, and clears it; ⇧↵ starts a new line instead of sending.
#[gpui::test]
fn the_message_to_the_orchestrator_sends_from_its_control(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let setup = setup(&view, cx);
    let (_, orchestrator) = setup.orchestrator;
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.show_board(orchestrator, true, cx);
    });
    cx.run_until_parked();
    let b = board(&view, cx, orchestrator);
    let line = |cx: &mut VisualTestContext| b.read_with(cx, ProjectView::composing);

    cx.simulate_keystrokes("c");
    cx.simulate_input("split #2");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("then merge");
    assert_eq!(line(cx).as_deref(), Some("split #2\nthen merge"), "a new line, not a send");
    assert_eq!(sent(&mut queue, cx, done), []);

    click(cx, "project-send");
    let verbs = sent(&mut queue, cx, done);
    let [Verb::TaskTell { task: None, text, .. }] = verbs.as_slice() else { panic!("{verbs:?}") };
    assert_eq!(text, "split #2\nthen merge");
    assert_eq!(line(cx).as_deref(), Some(""), "cleared once sent");
}

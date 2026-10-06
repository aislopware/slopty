//! One tile per agent: an agent in a live terminal is shown in that terminal's tile, its thread
//! as the tile's face, whether it was started here, came to a thread tile later or is opened
//! from elsewhere; a thread with no live terminal keeps a tile of its own.

use slopty_proto::thread::wire::{IntentDone, Outcome, TableFrame, ThreadRequest};
use slopty_proto::thread::{AgentId, Cursor, ThreadId, ThreadState};

use super::*;
use crate::workspace::faces::Face;

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// `states` as `key`'s whole thread table, at `seq`.
fn table(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    key: WorkerKey,
    seq: u64,
    states: &[&ThreadState],
) {
    let rows = states.iter().map(|s| s.row(WallMs::ZERO)).collect();
    let table = TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq }, rows };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table, cx));
    settle(cx);
}

/// The worker opened `session` with Claude Code working in it.
fn agent_runs_in(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    key: WorkerKey,
    session: SessionId,
) {
    view.update_in(cx, |v, _w, cx| {
        v.session_opened(key, summary(session, Some("/src/app")), cx);
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
    });
    cx.run_until_parked();
}

/// The item ops the workspace sent `fake`.
fn item_ops(fake: &mut Fake) -> Vec<ItemOp> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(op) => Some(op),
            _ => None,
        })
        .collect()
}

/// Whether `session`'s thread face holds the keyboard.
fn face_focused(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    session: SessionId,
) -> bool {
    cx.update(|window, cx| {
        view.read(cx)
            .thread_face(session)
            .is_some_and(|t| t.read(cx).focus_handle(cx).contains_focused(window, cx))
    })
}

/// What the tile's group says it shows, to a screen reader.
fn tile_says(cx: &mut VisualTestContext, title: &str) -> Option<String> {
    tree(cx).into_iter().find(|n| n.is("Group", Some(title))).and_then(|n| n.description)
}

/// Thread tile `item` for `thread`, opened here on `key` and echoed by its worker.
fn thread_tile(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &mut Fake,
    thread: ThreadId,
) -> TileRef {
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| v.open_thread(key, thread, cx));
    settle(cx);
    let ops = item_ops(fake);
    let [ItemOp::Add(item)] = ops.as_slice() else { panic!("one thread item: {ops:?}") };
    assert_eq!(item.kind, ItemKind::Thread { thread });
    TileRef { worker: key, item: item.id }
}

/// A Claude Code start lands in its terminal's tile, the start's own, on the thread face with
/// the keyboard in its composer; there is no thread tile. The tile says which face it shows,
/// and the header's toggle says what it turns to.
#[gpui::test]
fn a_tui_agents_start_lands_in_its_terminals_tile_on_the_thread_face(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    cx.run_until_parked();
    studio.drain();
    let claude = AgentId::named(AgentId::CLAUDE_CODE);
    view.update_in(cx, |v, _w, cx| v.start_thread(key, claude, "/src/app".into(), None, cx));
    let intent = studio
        .drain()
        .into_iter()
        .find_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { id, .. }) => Some(id),
            _ => None,
        })
        .expect("the start");
    settle(cx);
    let placeholder = focused(&view, cx).expect("the start's tile");

    // The worker opens the agent's terminal, its table names it, and the start is answered.
    let session = SessionId::new();
    agent_runs_in(&view, cx, key, session);
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let thread = state.meta.id;
    table(&view, cx, key, 1, &[&state]);
    let started = IntentDone { id: intent, outcome: Outcome::Started { thread } };
    view.update_in(cx, |v, _w, cx| v.thread_done(key, &started, cx));
    settle(cx);

    let ops = item_ops(&mut studio);
    let [ItemOp::Add(item)] = ops.as_slice() else { panic!("one item: {ops:?}") };
    assert_eq!(item.kind, ItemKind::Terminal { session }, "the terminal's tile, not a thread's");
    assert_eq!(item.id, placeholder.item, "in the start's own tile");
    let by = studio.me;
    let echo = ItemSync::Delta { version: 2, by, op: ItemOp::Add(item.clone()) };
    view.update_in(cx, |v, _w, cx| v.apply_sync(key, echo, cx));
    settle(cx);
    assert_eq!(focused(&view, cx), Some(placeholder));
    assert!(view.read_with(cx, |v, _| v.face_shown(session)), "on the thread face");
    assert!(face_focused(&view, cx, session), "the keyboard in its composer");
    assert_eq!(view.read_with(cx, |v, _| v.tile_of_thread(thread)), Some(placeholder));

    let title = view.read_with(cx, |v, _| v.tile_title(item));
    assert_eq!(tile_says(cx, &title).as_deref(), Some(Face::Thread.label()));
    // Under the pointer its header offers the other face.
    let header = cx.debug_bounds(selector("title", placeholder.item)).expect("its header");
    cx.simulate_mouse_move(header.center(), None, Modifiers::none());
    settle(cx);
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Button", Some("Show terminal"))), "{nodes:#?}");
    view.update_in(cx, |v, _w, cx| v.set_face(session, Face::Terminal, cx));
    settle(cx);
    assert_eq!(tile_says(cx, &title).as_deref(), Some(Face::Terminal.label()), "the face shown");
}

/// A thread tile whose agent comes to run in a terminal (taken up again, its TUI opened)
/// becomes that terminal's tile where it stood, under its id, and its thread view goes on as
/// the face: the draft and the keyboard are where they were.
#[gpui::test]
fn a_thread_tile_whose_agent_gains_a_terminal_becomes_its_tile_in_place(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    let thread = state.meta.id;
    table(&view, cx, key, 1, &[&state]);
    studio.drain();
    let tile = thread_tile(&view, cx, &mut studio, thread);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    settle(cx);
    let at = view.read_with(cx, |v, _| v.layout().position(tile)).expect("placed");
    let made = view.read_with(cx, |v, _| v.thread_item(tile.item).map(Entity::entity_id));
    cx.simulate_input("half a thought");
    cx.run_until_parked();

    let session = SessionId::new();
    agent_runs_in(&view, cx, key, session);
    state.meta.terminal = Some(session);
    table(&view, cx, key, 2, &[&state]);

    let ops = item_ops(&mut studio);
    let [ItemOp::Remove(gone), ItemOp::Add(item)] = ops.as_slice() else {
        panic!("the thread's item goes and the terminal's comes: {ops:?}")
    };
    assert_eq!((*gone, item.id), (tile.item, tile.item), "under the same id");
    assert_eq!(item.kind, ItemKind::Terminal { session });
    view.read_with(cx, |v, cx| {
        assert_eq!(v.layout().position(tile), Some(at), "where it stood");
        assert_eq!(v.focused(), Some(tile), "with the focus");
        let face = v.thread_face(session).expect("the thread face");
        assert_eq!(Some(face.entity_id()), made, "the same thread view");
        assert_eq!(face.read(cx).draft(cx), "half a thought", "its draft kept");
    });
    assert!(face_focused(&view, cx, session), "and the keyboard");
}

/// A thread whose terminal has a tile already goes there: its own tile leaves, and the
/// terminal's takes the focus.
#[gpui::test]
fn a_thread_whose_terminal_already_has_a_tile_goes_to_it(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let session = SessionId::new();
    let terminal = opens(&view, cx, &studio, session, studio.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    let thread = state.meta.id;
    table(&view, cx, key, 1, &[&state]);
    studio.drain();
    let tile = thread_tile(&view, cx, &mut studio, thread);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    settle(cx);

    state.meta.terminal = Some(session);
    table(&view, cx, key, 2, &[&state]);
    let ops = item_ops(&mut studio);
    assert_eq!(ops, [ItemOp::Remove(tile.item)], "only the thread's tile goes");
    view.read_with(cx, |v, _| {
        assert!(!v.layout().contains(tile), "gone from the layout");
        assert_eq!(v.focused(), Some(terminal), "the terminal's tile has the focus");
        assert_eq!(v.tile_of_thread(thread), Some(terminal));
    });
}

/// A thread with no live terminal keeps a tile of its own: one whose agent runs in none (an
/// ACP agent, Codex with no TUI open), and one whose agent's program has exited.
#[gpui::test]
fn a_thread_with_no_live_terminal_keeps_its_own_tile(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let free = crate::conversation::thread::fixtures::thread("edit");
    let mut ended = crate::conversation::thread::fixtures::thread("edit");
    ended.meta.id = ThreadId::new();
    let session = SessionId::new();
    ended.meta.terminal = Some(session);
    let mut exited = summary(session, None);
    exited.state = SessionState::Exited { status: 0 };
    view.update_in(cx, |v, _w, cx| v.session_opened(key, exited, cx));
    table(&view, cx, key, 1, &[&free, &ended]);
    studio.drain();

    let free_tile = thread_tile(&view, cx, &mut studio, free.meta.id);
    let ended_tile = thread_tile(&view, cx, &mut studio, ended.meta.id);
    table(&view, cx, key, 2, &[&free, &ended]);
    assert!(item_ops(&mut studio).is_empty(), "nothing moves");
    view.read_with(cx, |v, _| {
        for tile in [free_tile, ended_tile] {
            assert!(matches!(v.item(tile).map(|i| &i.kind), Some(ItemKind::Thread { .. })));
        }
    });
}

/// Opening a thread whose agent runs in a live terminal (from the navigator, a note, a
/// notice) opens that terminal's tile on its face, or goes to the one there is.
#[gpui::test]
fn opening_an_agents_thread_opens_its_terminals_tile(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let session = SessionId::new();
    agent_runs_in(&view, cx, key, session);
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let thread = state.meta.id;
    table(&view, cx, key, 1, &[&state]);
    studio.drain();

    view.update_in(cx, |v, _w, cx| v.open_thread(key, thread, cx));
    settle(cx);
    let ops = item_ops(&mut studio);
    let [ItemOp::Add(item)] = ops.as_slice() else { panic!("one item: {ops:?}") };
    assert_eq!(item.kind, ItemKind::Terminal { session });
    let tile = TileRef { worker: key, item: item.id };
    assert_eq!(focused(&view, cx), Some(tile));
    assert!(face_focused(&view, cx, session), "on its face, the keyboard in its composer");

    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 3);
    studio.drain();
    view.update_in(cx, |v, _w, cx| v.open_thread(key, thread, cx));
    settle(cx);
    assert!(item_ops(&mut studio).is_empty(), "no second tile");
    assert_eq!(focused(&view, cx), Some(tile), "the one there is");
}

/// An agent that exited leaves its tile; taken up again in another terminal, it goes on in that
/// tile, where it stood and under its id, on the face the person left it on. The exited
/// session's last screen is closed on its worker.
#[gpui::test]
fn an_exited_agents_tile_goes_on_where_its_thread_is_taken_up_again(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let gone = SessionId::new();
    let tile = opens(&view, cx, &studio, gone, studio.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(gone) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(gone);
    let thread = state.meta.id;
    table(&view, cx, key, 1, &[&state]);
    view.update_in(cx, |v, _w, cx| {
        v.show_face(gone, false, cx);
        v.focus_tile(tile, cx);
    });
    let mut exited = summary(gone, None);
    exited.state = SessionState::Exited { status: 0 };
    view.update_in(cx, |v, _w, cx| v.session_opened(key, exited, cx));
    settle(cx);
    studio.drain();

    let again = SessionId::new();
    agent_runs_in(&view, cx, key, again);
    state.meta.terminal = Some(again);
    table(&view, cx, key, 2, &[&state]);
    let sent = studio.drain();
    let ops: Vec<&ItemOp> = sent
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Items(op) => Some(op),
            _ => None,
        })
        .collect();
    let [ItemOp::Remove(left), ItemOp::Add(item)] = ops.as_slice() else {
        panic!("the exited terminal's item gives way to the new one's: {ops:?}")
    };
    assert_eq!((*left, item.id), (tile.item, tile.item), "under the same id");
    assert_eq!(item.kind, ItemKind::Terminal { session: again });
    let closed = sent.iter().any(
        |m| matches!(m, ClientMsg::Term { session, req: TermRequest::Close } if *session == gone),
    );
    assert!(closed, "the exited session is closed: {sent:?}");
    view.read_with(cx, |v, _| {
        assert_eq!(v.focused(), Some(tile), "with the focus, where it stood");
        assert!(!v.face_shown(again), "on the TUI, as the person left it");
        assert_eq!(v.tile_of_thread(thread), Some(tile));
    });
}

/// Identity leads, state trails: an agent's tile leads with its agent's own mark whether it
/// works, needs the person or rests, in its header and its navigator row, and the mark names
/// the agent to a screen reader; how it is doing takes the row's end, and goes at rest. The
/// place no longer names the agent in words. A plain shell keeps its own glyph.
#[gpui::test]
fn an_agents_tile_leads_with_its_mark_and_ends_with_its_state(cx: &mut TestAppContext) {
    use crate::icons::{AgentMark, Mark};
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    agent_runs_in(&view, cx, key, session);
    let mut claude = crate::conversation::thread::fixtures::thread("edit");
    claude.meta.terminal = Some(session);
    claude.meta.agent = AgentId::named(AgentId::CLAUDE_CODE);
    table(&view, cx, key, 1, &[&claude]);

    let read = |cx: &mut VisualTestContext, tile: TileRef| {
        view.read_with(cx, |v, _| {
            let item = v.item(tile).expect("the tile");
            (v.kind_glyph(item), v.tile_place(item))
        })
    };
    let spark = Mark::Agent(AgentMark::Claude);
    let ends = |cx: &mut VisualTestContext, what: &str| {
        settle(cx);
        let lead = cx.debug_bounds(selector("nav-kind", tile.item)).expect("the row's lead");
        let row = cx.debug_bounds(selector("nav-tile", tile.item)).expect("the row");
        let state = cx.debug_bounds(selector("nav-state", tile.item));
        assert!(lead.right() < row.center().x, "{what}: the mark leads: {lead:?} {row:?}");
        assert_eq!(read(cx, tile).0, spark, "{what}: the mark never changes");
        state.map(|state| {
            let word = cx.debug_bounds(selector("nav-word", tile.item)).expect("its word");
            assert!(word.left() > row.center().x, "{what}: the state ends it: {word:?}");
            assert!(state.right() <= word.left(), "{what}: the glyph, then its word: {state:?}");
            assert!(word.right() <= row.right(), "{what}: {word:?} {row:?}");
            assert!(state.top() > lead.bottom() - px(0.5), "{what}: on the second line: {state:?}");
        })
    };
    let works = AgentEvent { status: AgentStatus::Working, ..blocked(session) };
    view.update_in(cx, |v, _w, cx| v.agent_event(works, cx));
    assert!(ends(cx, "working").is_some(), "a working agent ends its row with the spinner");
    let place = read(cx, tile).1.unwrap_or_default();
    assert!(!place.contains("Claude Code"), "the mark says which agent: {place}");
    let header = cx.debug_bounds(selector("kind", tile.item)).expect("the header's lead");
    let status = cx.debug_bounds(selector("status", tile.item)).expect("the header's state");
    assert!(header.right() < status.left(), "the header's state ends it: {header:?} {status:?}");
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Image", Some("Claude Code"))), "named: {nodes:#?}");

    let asks = AgentEvent {
        status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
        ..blocked(session)
    };
    view.update_in(cx, |v, _w, cx| v.agent_event(asks, cx));
    assert!(ends(cx, "needs you").is_some(), "needing the person ends the row with its mark");
    let rests = AgentEvent { status: AgentStatus::Idle, ..blocked(session) };
    view.update_in(cx, |v, _w, cx| v.agent_event(rests, cx));
    assert!(ends(cx, "at rest").is_none(), "at rest the row's end says no state");

    let (glyph, place) = read(cx, shell);
    assert_eq!(glyph, Mark::Symbol(crate::icons::Symbol::Terminal));
    assert!(!place.unwrap_or_default().contains("Claude Code"), "a shell names no agent");

    // An agent with no mark of its own wears the neutral glyph and is named in words.
    let other = SessionId::new();
    let acp = opens(&view, cx, &studio, other, studio.me, 3);
    agent_runs_in(&view, cx, key, other);
    let mut opencode = crate::conversation::thread::fixtures::thread("plan");
    opencode.meta.terminal = Some(other);
    opencode.meta.agent = AgentId::acp("opencode");
    table(&view, cx, key, 2, &[&claude, &opencode]);
    let (glyph, place) = read(cx, acp);
    assert_eq!(glyph, Mark::Symbol(crate::icons::AGENT), "the neutral glyph");
    let title = view.read_with(cx, |v, _| v.tile_title(v.item(acp).expect("the tile")));
    let place = place.unwrap_or_default();
    assert!(
        place.starts_with("opencode") || title.starts_with("opencode"),
        "named in words: {title} / {place}"
    );
}

/// A thread is called by its work, whichever agent runs it: the title its worker's table gives
/// (the agent's own name for the session, else its first prompt, as every adapter fills it),
/// and before there is one its agent's name, never a bare "Thread". With its mark leading,
/// mark and title read as one identity.
#[gpui::test]
fn a_thread_is_titled_by_its_work_else_by_its_agent(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let mut codex = crate::conversation::thread::fixtures::thread("edit");
    codex.meta.agent = AgentId::named(AgentId::CODEX);
    codex.meta.terminal = None;
    codex.meta.title = String::new();
    let thread = codex.meta.id;
    let tile = arrives(&view, cx, &studio, ItemKind::Thread { thread }, 1);
    let title = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.tile_title(v.item(tile).expect("the tile")))
    };
    table(&view, cx, key, 1, &[&codex]);
    assert_eq!(title(cx), "Codex", "named by its agent before its work");
    codex.meta.title = "Fix the parser's error spans".to_owned();
    table(&view, cx, key, 2, &[&codex]);
    assert_eq!(title(cx), "Fix the parser's error spans", "then by its work");
}

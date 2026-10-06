//! An agent tile whose thread the worker's table names draws that thread's view as its face,
//! under the tile's own header, and gives it the keyboard.

use slopty_core::WallMs;
use slopty_proto::thread::Cursor;
use slopty_proto::thread::wire::TableFrame;

use super::*;

/// Known to the table, the agent's thread is drawn in its tile in place of its TUI, with no
/// header of its own (the tile's says the same), and what is typed goes to its
/// composer.
#[gpui::test]
fn an_agent_tile_whose_thread_is_known_draws_and_focuses_its_thread_view(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| {
        v.thread_table(key, &table, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    let body = cx.debug_bounds(selector("item", tile.item)).expect("the tile");
    let composer = cx.debug_bounds("thread-composer").expect("the thread view's composer");
    assert!(body.contains(&composer.center()), "in the tile: {body:?} {composer:?}");
    assert!(cx.debug_bounds("thread-header").is_none(), "the tile's header says it once");

    cx.simulate_input("ship it");
    cx.run_until_parked();
    let draft = view.read_with(cx, |v, cx| v.thread_face(session).map(|t| t.read(cx).draft(cx)));
    assert_eq!(draft.as_deref(), Some("ship it"), "the keyboard is the thread view's");
}

/// A thread arriving for an agent in another tile turns that tile to its thread view and
/// leaves the keyboard where it is: in the focused tile's shell.
#[gpui::test]
fn a_thread_arriving_elsewhere_leaves_the_keyboard_in_the_focused_shell(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let agent = SessionId::new();
    let _agent_tile = opens(&view, cx, &studio, agent, studio.me, 1);
    let shell = SessionId::new();
    let shell_tile = opens(&view, cx, &studio, shell, studio.me, 2);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.focus_tile(shell_tile, cx);
        v.agent_event(blocked(agent), cx);
        v.threads_linked(key, cx);
    });
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, shell), "the shell holds the keyboard first");
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(agent);
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.thread_face(agent).is_some()), "the agent's thread view");
    assert!(terminal_focused(&view, cx, shell), "the keyboard stays in the focused shell");
}

/// A thread that moves to another terminal (its agent's session taken up there) takes its view
/// out of the focused tile, and the keyboard stays with that tile: back in its shell, never with
/// nothing.
#[gpui::test]
fn a_thread_that_moves_away_hands_the_keyboard_back_to_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (here, there) = (SessionId::new(), SessionId::new());
    let _there_tile = opens(&view, cx, &studio, there, studio.me, 1);
    let here_tile = opens(&view, cx, &studio, here, studio.me, 2);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(blocked(there), cx);
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(here) }, cx);
        v.threads_linked(key, cx);
        v.focus_tile(here_tile, cx);
    });
    cx.run_until_parked();
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    let table = |state: &slopty_proto::thread::ThreadState, seq| TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq },
        rows: vec![state.row(WallMs::ZERO)],
    };
    state.meta.terminal = Some(here);
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table(&state, 1), cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let composer = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            view.read(cx)
                .thread_face(here)
                .is_some_and(|t| t.read(cx).focus_handle(cx).contains_focused(window, cx))
        })
    };
    assert!(composer(cx), "the focused tile's thread view has the keyboard");

    state.meta.terminal = Some(there);
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table(&state, 2), cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.thread_face(here).is_none()), "the thread went");
    assert!(terminal_focused(&view, cx, here), "the keyboard is back in the tile's shell");
}

/// A tile showing its TUI with the keyboard keeps the keyboard when the thread of the agent
/// started there becomes known and its view takes the TUI's place.
#[gpui::test]
fn the_thread_view_takes_the_keyboard_from_the_tui_it_replaces(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, session), "the TUI has the keyboard first");

    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let thread_has_it = cx.update(|window, cx| {
        view.read(cx)
            .thread_face(session)
            .is_some_and(|t| t.read(cx).focus_handle(cx).contains_focused(window, cx))
    });
    assert!(thread_has_it, "the thread view has it now");
}

/// A tile showing a thread says where its agent works after the title, the checkout then the
/// branch, in its header; the shell's directory beside the title would say it again, so it
/// goes.
#[gpui::test]
fn a_thread_tile_says_its_checkout_and_branch_in_its_header(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens_in(&view, cx, &studio, session, studio.me, 1, Some("/src/slopty"));
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    state.meta.cwd = "/src/slopty".to_owned();
    state.meta.facts.insert("branch".to_owned(), "responsive-headers".to_owned());
    let thread = state.meta.id;
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| {
        v.thread_table(key, &table, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    let snapshot = slopty_proto::thread::wire::ThreadFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 40 },
        state: Box::new(state),
    };
    view.update_in(cx, |v, _w, cx| v.thread_frame(key, thread, snapshot, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    let header = cx.debug_bounds(selector("title", tile.item)).expect("the header");
    let checkout = cx.debug_bounds(selector("thread-checkout", tile.item)).expect("the checkout");
    let branch = cx.debug_bounds(selector("thread-branch", tile.item)).expect("the branch");
    assert!(header.contains(&checkout.center()) && header.contains(&branch.center()));
    let name = cx.debug_bounds(selector("name", tile.item)).expect("the title");
    assert!(name.right() <= checkout.left(), "after the title: {name:?} {checkout:?}");
    assert!(checkout.right() <= branch.left(), "the checkout, then the branch");
    assert!(cx.debug_bounds(selector("place", tile.item)).is_none(), "said once");
    let nodes = tree(cx);
    assert!(
        nodes.iter().any(|n| n.label.as_deref() == Some("slopty")
            || n.label.as_deref() == Some("Commit on slopty")),
        "{nodes:#?}"
    );
}

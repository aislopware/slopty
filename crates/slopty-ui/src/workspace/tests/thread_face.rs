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

/// A tile showing a thread says where its agent works once, in the composer's foot (the
/// checkout and its branch): its header keeps to its title, and neither the checkout nor the
/// shell's directory stands beside it.
#[gpui::test]
fn a_thread_tile_says_its_checkout_once_in_the_composers_foot(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens_in(&view, cx, &studio, session, studio.me, 1, Some("/src/slopty"));
    paired(&view, cx, &studio, tile, 2);
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
    assert!(cx.debug_bounds(selector("thread-checkout", tile.item)).is_none(), "not in the header");
    assert!(cx.debug_bounds(selector("thread-branch", tile.item)).is_none());
    assert!(cx.debug_bounds(selector("place", tile.item)).is_none(), "nor the directory");
    let foot = cx.debug_bounds("thread-checkout").expect("the composer's foot says it");
    assert!(foot.top() >= header.bottom(), "under the header: {foot:?}");
    let nodes = tree(cx);
    assert!(
        nodes.iter().any(|n| n.label.as_deref() == Some("Works in slopty / responsive-headers")),
        "{nodes:#?}"
    );
}

/// "!" first in a thread's empty composer opens a shell on the thread's machine, in its folder,
/// off the tiling: no tab, no pane; the shell stands in the thread's next-turn place with the
/// keyboard. The next message goes led by what it shows, and the shell ends with it.
#[gpui::test]
fn a_bang_opens_a_shell_in_the_thread_and_the_next_message_carries_it(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::{Intent, ThreadFrame, ThreadRequest};

    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    state.meta.cwd = "/w/app".to_owned();
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
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    // The thread itself, once its face has asked for it: its folder is what "!" opens in.
    let frame_of =
        ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, state: Box::new(state) };
    view.update_in(cx, |v, _w, cx| v.thread_frame(key, thread, frame_of, cx));
    cx.run_until_parked();
    studio.drain();
    let tabs = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.layout().shown_project().map(|p| p.tabs().len()))
    };
    let before = tabs(cx);

    cx.simulate_input("!");
    cx.run_until_parked();
    let draft = view.read_with(cx, |v, cx| v.thread_face(session).map(|t| t.read(cx).draft(cx)));
    assert_eq!(draft.as_deref(), Some(""), "the ! is not typed");
    let sent = studio.drain();
    assert!(
        sent.iter().any(|m| matches!(
            m,
            ClientMsg::OpenSession { spec: OpenSession { cwd: Some(cwd), .. }, .. } if cwd == "/w/app"
        )),
        "a shell in the thread's folder: {sent:?}"
    );
    assert!(cx.debug_bounds("inline-terminal").is_some(), "its place shows at once");

    let shell = SessionId::new();
    let inline = opens_in(&view, cx, &studio, shell, studio.me, 2, Some("/w/app"));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.layout().contains(inline)), "off the tiling");
    assert_eq!(tabs(cx), before, "no tab of its own");
    assert!(terminal_focused(&view, cx, shell), "the shell has the keyboard");
    let card = cx.debug_bounds("inline-terminal").expect("the card");
    let body = cx.debug_bounds(selector("item", tile.item)).expect("the tile");
    assert!(body.contains(&card.center()), "in the thread's tile: {body:?} {card:?}");

    view.update_in(cx, |v, _w, cx| {
        v.term_event(shell, frame(&["~/app % ls", "Cargo.toml  src", "~/app %", ""]), cx);
    });
    cx.run_until_parked();
    studio.drain();
    // Back to the composer by a press on its field, as the person goes.
    let composer = cx.debug_bounds("thread-composer-field").expect("the composer");
    cx.simulate_click(composer.center(), Modifiers::none());
    cx.simulate_input("why");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let sent = studio.drain();
    let text = sent
        .iter()
        .find_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent {
                intent: Intent::Send { text, .. }, ..
            }) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a message went: {sent:?}"));
    assert!(
        text.starts_with("```console\n~/app % ls\nCargo.toml  src") && text.ends_with("```\n\nwhy"),
        "led by what the shell shows: {text:?}"
    );
    assert!(
        sent.iter().any(|m| matches!(
            m,
            ClientMsg::Term { session: s, req: TermRequest::Close } if *s == shell
        )),
        "the shell ends: {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|m| matches!(m, ClientMsg::Items(ItemOp::Remove(id)) if *id == inline.item)),
        "and its item goes: {sent:?}"
    );
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds("inline-terminal").is_none(), "its place goes");
}

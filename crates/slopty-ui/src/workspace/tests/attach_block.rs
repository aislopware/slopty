//! A shell's command block handed to an agent as context: the thread it goes to, whatever the
//! agent and whichever tile holds it, and that it lands in the thread's draft.

use super::*;
use crate::terminal::TerminalViewEvent;

/// A shell's block goes to the agent tile focused last on the same worker: that tile comes up
/// on its thread, and the draft holds the block's Markdown after what was typed there. With no
/// agent on the worker, the shell offers nothing to attach to.
#[gpui::test]
fn a_block_from_a_shell_lands_in_the_last_agents_draft(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let shell = SessionId::new();
    let shell_tile = opens(&view, cx, &studio, shell, studio.me, 1);
    cx.run_until_parked();
    let target = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.block_target(shell));
    assert_eq!(target(cx), None, "no agent on the worker");

    let agent = SessionId::new();
    let agent_tile = opens(&view, cx, &studio, agent, studio.me, 2);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(agent) }, cx);
        v.focus_tile(agent_tile, cx);
    });
    cx.run_until_parked();
    // A draft under way on the agent's thread.
    let thread = agent_thread(&view, cx, studio.key, agent);
    cx.simulate_input("look at this");
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell_tile, cx));
    cx.run_until_parked();
    studio.drain();
    assert_eq!(target(cx), Some(thread));

    let block = "The terminal ran `false`, which exited 1:\n\n(no output)\n";
    let terminal = view.read_with(cx, |v, _| v.terminals.get(&shell).cloned()).expect("drawn");
    terminal.update(cx, |_, cx| cx.emit(TerminalViewEvent::Attach(block.to_owned())));
    cx.run_until_parked();
    let face = view.read_with(cx, |v, _| v.thread_face(agent).cloned()).expect("its thread");
    let draft = face.read_with(cx, crate::conversation::thread::ThreadView::draft);
    assert_eq!(draft, format!("look at this\n\n{block}"));
    assert_eq!(focused(&view, cx), Some(agent_tile), "the agent's tile comes forward");
    assert!(view.read_with(cx, |v, _| v.face_shown(agent)), "on its thread");
}

/// A thread with no terminal (an agent Slopty drives over its protocol, as pi or Codex) takes
/// a shell's block as a terminal's agent does: its own tile, focused last on the worker, is
/// the target, and the block lands in its draft. Once its agent has exited, it is no target.
#[gpui::test]
fn a_block_goes_to_a_thread_tile_whatever_its_agent(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::TableFrame;
    use slopty_proto::thread::{AgentId, Cursor, Drive, Liveness};

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let shell = SessionId::new();
    let shell_tile = opens(&view, cx, &studio, shell, studio.me, 1);
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.agent = AgentId::named(AgentId::PI);
    state.meta.drive = Drive::named(Drive::DRIVEN);
    let thread = state.meta.id;
    let table = |state: &slopty_proto::thread::ThreadState, seq| TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        v.thread_table(key, &table(&state, 1), cx);
        v.open_thread(key, thread, cx);
    });
    cx.run_until_parked();
    let added = studio.drain().into_iter().find_map(|m| match m {
        ClientMsg::Items(ItemOp::Add(item)) if item.kind == ItemKind::Thread { thread } => {
            Some(item.id)
        }
        _ => None,
    });
    let item = added.expect("a tile of the thread's own");
    let tile = TileRef { worker: key, item };
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell_tile, cx));
    cx.run_until_parked();
    let target = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.block_target(shell));
    assert_eq!(target(cx), Some(thread), "the thread tile focused last");

    let block = "The terminal ran `true`, which exited 0:\n\n(no output)\n";
    let terminal = view.read_with(cx, |v, _| v.terminals.get(&shell).cloned()).expect("drawn");
    terminal.update(cx, |_, cx| cx.emit(TerminalViewEvent::Attach(block.to_owned())));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let own = view.read_with(cx, |v, _| v.thread_item(item).cloned());
    let draft =
        own.expect("its view").read_with(cx, crate::conversation::thread::ThreadView::draft);
    assert_eq!(draft, block);
    assert_eq!(focused(&view, cx), Some(tile), "the thread's tile comes forward");

    state.status.liveness = Liveness::Exited { resumable: true };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table(&state, 2), cx));
    cx.run_until_parked();
    assert_eq!(target(cx), None, "an exited agent takes no block");
}

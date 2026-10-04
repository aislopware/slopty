//! A shell's command block handed to an agent as context: where it goes, and that it lands in
//! the agent's draft.

use super::*;
use crate::conversation::ConversationView;
use crate::terminal::TerminalViewEvent;

/// A shell's block goes to the agent tile focused last on the same worker: that tile comes up
/// on its face, and the draft holds the block's Markdown after what was typed there. With no
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
    // A draft under way on the agent's face.
    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    cx.simulate_input("look at this");
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell_tile, cx));
    cx.run_until_parked();
    studio.drain();
    assert_eq!(target(cx), Some(agent));

    let block = "The terminal ran `false`, which exited 1:\n\n(no output)\n";
    let terminal = view.read_with(cx, |v, _| v.terminals.get(&shell).cloned()).expect("drawn");
    terminal.update(cx, |_, cx| cx.emit(TerminalViewEvent::Attach(block.to_owned())));
    cx.run_until_parked();
    let face = view.read_with(cx, |v, _| v.conversation(agent).cloned()).expect("a face");
    let draft = face.read_with(cx, ConversationView::draft);
    assert_eq!(draft, format!("look at this\n\n{block}"));
    assert_eq!(focused(&view, cx), Some(agent_tile), "the agent's tile comes forward");
    assert!(view.read_with(cx, |v, _| v.face_shown(agent)), "on its face");
}

//! The focused tile under the view cache: replayed while other tiles draw; drawn afresh for its
//! own changes, when the focus moves, for what is typed into a thread's or a note's field, and with
//! every frame while a shell's find bar has the keyboard.

use super::*;

/// 80 columns of words, `rows` deep.
fn dense(rows: usize, seed: usize) -> Vec<String> {
    (0..rows).map(|row| format!("{:<80}", format!("{seed} {row} ").repeat(12))).collect()
}

/// A focused shell full of text is left as it was drawn while a neighbour's output draws a
/// hundred frames; its own output draws it, and so do the focus leaving and coming back.
#[gpui::test]
fn the_focused_shell_is_replayed_while_a_neighbour_draws(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(busy, beside), (quiet, mine), _] = three_shells(&view, cx, &fake);
    let screen = dense(40, 0);
    let screen: Vec<&str> = screen.iter().map(String::as_str).collect();
    view.update_in(cx, |v, _w, cx| {
        v.term_event(quiet, frame(&screen), cx);
        v.focus_tile(mine, cx);
    });
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, quiet), "the dense shell has the keyboard");
    assert!(cx.debug_bounds(selector("item", beside.item)).is_some(), "the neighbour is drawn");
    let term = |s: SessionId, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.terminal(s).cloned()).expect("attached")
    };
    let (focused, loud) = (term(quiet, cx), term(busy, cx));
    let renders =
        |t: &Entity<TerminalView>, cx: &mut VisualTestContext| t.read_with(cx, |t, _| t.renders());
    // GPUI draws the whole window afresh once after a focus lands.
    view.update_in(cx, |v, _w, cx| v.term_event(busy, frame(&["line", ""]), cx));
    cx.run_until_parked();
    let (before, loud_before) = (renders(&focused, cx), renders(&loud, cx));
    for i in 0..100 {
        let line = format!("line {i}");
        view.update_in(cx, |v, _w, cx| v.term_event(busy, frame(&[&line, ""]), cx));
        cx.run_until_parked();
    }
    assert!(renders(&loud, cx) >= loud_before.saturating_add(100), "the neighbour drew");
    assert_eq!(renders(&focused, cx), before, "the focused shell was replayed, not rendered");

    let screen = dense(40, 1);
    let screen: Vec<&str> = screen.iter().map(String::as_str).collect();
    view.update_in(cx, |v, _w, cx| v.term_event(quiet, frame(&screen), cx));
    cx.run_until_parked();
    let own = renders(&focused, cx);
    assert!(own > before, "its own output draws it: {before} → {own}");

    view.update_in(cx, |v, _w, cx| v.focus_tile(beside, cx));
    cx.run_until_parked();
    let left = renders(&focused, cx);
    assert!(left > own, "the focus leaving draws it: {own} → {left}");
    view.update_in(cx, |v, _w, cx| v.focus_tile(mine, cx));
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, quiet));
    assert!(renders(&focused, cx) > left, "and coming back");
}

/// While the focused shell's find bar has the keyboard, the shell is still left as it was
/// drawn while a neighbour's output draws frames: the field is a view, so what is typed there
/// draws the shell.
#[gpui::test]
fn a_focused_shell_whose_find_bar_has_the_keys_is_replayed_until_it_is_typed_in(
    cx: &mut TestAppContext,
) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(busy, _), (quiet, mine), _] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(mine, cx));
    cx.run_until_parked();
    let focused = view.read_with(cx, |v, _| v.terminal(quiet).cloned()).expect("attached");
    let line = crate::kit::find::Query { needle: "line".to_owned(), ..Default::default() };
    cx.update(|window, cx| {
        focused.update(cx, |t, cx| {
            t.find_with(&line, window, cx);
        });
    });
    cx.run_until_parked();
    assert!(!terminal_focused(&view, cx, quiet), "the find bar has the keyboard");
    // The keyboard moved inside the tile since its body was drawn, so the next frame draws the
    // shell once more; a replay would hand keys to the grid.
    view.update_in(cx, |v, _w, cx| v.term_event(busy, frame(&["line", ""]), cx));
    cx.run_until_parked();
    let before = focused.read_with(cx, |t, _| t.renders());
    for i in 0..100 {
        let line = format!("line {i}");
        view.update_in(cx, |v, _w, cx| v.term_event(busy, frame(&[&line, ""]), cx));
        cx.run_until_parked();
    }
    assert_eq!(focused.read_with(cx, |t, _| t.renders()), before, "replayed");
    cx.simulate_input(" 7");
    cx.run_until_parked();
    let typed = focused.read_with(cx, |t, _| t.renders());
    assert!(typed > before, "the typing draws the shell: {before} → {typed}");
}

/// A focused thread, its composer holding the keyboard, is left as it was drawn while a
/// neighbour's output draws a hundred frames. The thread observes its composer, so what is
/// typed there draws it.
#[gpui::test]
fn the_focused_thread_is_replayed_while_a_neighbour_draws(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let busy = SessionId::new();
    let beside = opens(&view, cx, &fake, busy, fake.me, 1);
    let agent = SessionId::new();
    let mine = opens(&view, cx, &fake, agent, fake.me, 2);
    view.update_in(cx, |v, _w, cx| {
        let idle = AgentEvent { status: AgentStatus::Idle, attention: false, ..blocked(agent) };
        v.agent_event(idle, cx);
        v.focus_tile(mine, cx);
    });
    cx.run_until_parked();
    agent_thread(&view, cx, fake.key, agent);
    let face = view.read_with(cx, |v, _| v.thread_face(agent).cloned()).expect("its thread");
    let composing =
        cx.update(|window, cx| face.read(cx).focus_handle(cx).contains_focused(window, cx));
    assert!(composing, "the composer has the keyboard");
    assert!(cx.debug_bounds(selector("item", beside.item)).is_some(), "the neighbour is drawn");
    let renders = |cx: &mut VisualTestContext| face.read_with(cx, |f, _| f.renders());
    // GPUI draws the whole window afresh once after a focus lands.
    view.update_in(cx, |v, _w, cx| v.term_event(busy, frame(&["line", ""]), cx));
    cx.run_until_parked();
    let before = renders(cx);
    for i in 0..100 {
        let line = format!("line {i}");
        view.update_in(cx, |v, _w, cx| v.term_event(busy, frame(&[&line, ""]), cx));
        cx.run_until_parked();
    }
    assert_eq!(renders(cx), before, "the focused thread was replayed, not rendered");

    cx.simulate_input("ship it");
    cx.run_until_parked();
    assert!(renders(cx) > before, "typing draws the thread");
    assert_eq!(face.read_with(cx, crate::conversation::thread::ThreadView::draft), "ship it");
}

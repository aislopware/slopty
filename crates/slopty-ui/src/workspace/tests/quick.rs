//! The quick terminal: one shell, opened the first time on the worker and in the directory of
//! the shell used last, drawn in a panel of its own that shows and hides with nothing reaching
//! the worker, and put away when it loses the keyboard, on Esc with no shell in it, or when its
//! shell ends.

use std::time::Instant;

use gpui::{AnyWindowHandle, EntityId, WindowHandle};

use super::*;
use crate::quick_terminal::{Body, QuickTerminalView};

fn panel(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> WindowHandle<QuickTerminalView> {
    view.read_with(cx, |v, _| v.quick_terminal_window())
        .and_then(|w| w.downcast::<QuickTerminalView>())
        .expect("the quick terminal's panel")
}

/// Whether the panel is shown, and the terminal it draws.
fn state(
    panel: WindowHandle<QuickTerminalView>,
    cx: &mut VisualTestContext,
) -> (bool, Option<EntityId>) {
    panel
        .update(cx, |q, _window, _cx| {
            let drawn = match q.body() {
                Body::Terminal(view) => Some(view.entity_id()),
                Body::Waiting(..) => None,
            };
            (q.is_shown(), drawn)
        })
        .expect("open")
}

fn toggle(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
    view.update_in(cx, |v, _window, cx| {
        v.toggle_quick_terminal(Instant::now(), QuickToggle::Chord, cx);
    });
    cx.run_until_parked();
}

/// The sessions asked for, by their directory.
fn asked(msgs: &[ClientMsg]) -> Vec<Option<String>> {
    msgs.iter()
        .filter_map(|m| match m {
            ClientMsg::OpenSession { spec, .. } => Some(spec.cwd.clone()),
            _ => None,
        })
        .collect()
}

/// A worker with a focused shell in `/Users/me/src`, and the quick terminal toggled once and
/// answered: the quick shell's session and tile, and the shell's tile.
fn with_quick_shell(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &mut Fake,
) -> (SessionId, TileRef, TileRef) {
    let shell = opens_in(view, cx, fake, SessionId::new(), fake.me, 1, Some("/Users/me/src"));
    view.update_in(cx, |v, _window, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    fake.drain();
    toggle(view, cx);
    assert_eq!(asked(&fake.drain()), [Some("/Users/me/src".to_owned())], "the last shell's");
    let session = SessionId::new();
    let quick = opens(view, cx, fake, session, fake.me, 2);
    (session, quick, shell)
}

/// The first toggle opens a shell where the last one was used and shows it in the panel while
/// the focus stays put; the tile says where it went. Hiding and showing again draw the same
/// terminal, and the worker hears nothing of either.
#[gpui::test]
fn the_quick_terminal_keeps_its_shell_across_shows(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let (session, quick, shell) = with_quick_shell(&view, cx, &mut fake);
    let panel = panel(&view, cx);
    let term = view.read_with(cx, |v, _| v.terminal(session).cloned()).expect("attached");
    assert_eq!(state(panel, cx), (true, Some(term.entity_id())), "the workspace's own view");
    assert_eq!(focused(&view, cx), Some(shell), "the focus stayed where it was");
    let focused_in_panel = panel
        .update(cx, |_q, window, cx| term.read(cx).focus_handle(cx).is_focused(window))
        .expect("open");
    assert!(focused_in_panel, "the shell has the keyboard");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("waiting", quick.item)).is_some(), "its tile says where");

    toggle(&view, cx);
    assert_eq!(state(panel, cx), (false, Some(term.entity_id())), "hidden, the shell kept");
    toggle(&view, cx);
    assert_eq!(state(panel, cx), (true, Some(term.entity_id())), "the same shell again");
    let sent = fake.drain();
    assert!(asked(&sent).is_empty(), "no second shell: {sent:?}");
    assert!(
        !sent.iter().any(|m| matches!(
            m,
            ClientMsg::Term { req: TermRequest::Close | TermRequest::Detach, .. }
        )),
        "nothing closed or let go: {sent:?}"
    );
    let windows = cx.update(|_, cx| cx.windows().len());
    toggle(&view, cx);
    toggle(&view, cx);
    assert_eq!(cx.update(|_, cx| cx.windows().len()), windows, "one panel, kept");
}

/// With "Hide when unfocused" on, the panel goes when another window takes the keyboard; off,
/// it stays.
#[gpui::test]
fn the_quick_terminal_hides_when_it_loses_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let _quick = with_quick_shell(&view, cx, &mut fake);
    let handle = panel(&view, cx);
    let away = |cx: &mut VisualTestContext| {
        let panel_window: AnyWindowHandle = handle.into();
        VisualTestContext::from_window(panel_window, cx).deactivate_window();
        cx.run_until_parked();
    };
    away(cx);
    assert!(!state(handle, cx).0, "hidden once the keyboard went elsewhere");

    let config = QuickConfig { autohide: false, ..QuickConfig::default() };
    view.update_in(cx, |v, _window, cx| v.set_quick_terminal(config, cx));
    toggle(&view, cx);
    assert!(state(handle, cx).0);
    away(cx);
    assert!(state(handle, cx).0, "kept up with autohide off");
}

/// Esc puts the panel away when no shell holds the keyboard (none to open one on yet), and
/// goes to the shell when one does.
#[gpui::test]
fn esc_hides_the_quick_terminal_only_without_a_shell_in_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    toggle(&view, cx);
    let handle = panel(&view, cx);
    assert_eq!(state(handle, cx), (true, None), "no worker: a line saying so");
    let panel_window: AnyWindowHandle = handle.into();
    let mut in_panel = VisualTestContext::from_window(panel_window, cx);
    in_panel.simulate_keystrokes("escape");
    assert!(!state(handle, cx).0, "Esc put it away");

    let mut fake = connect(&view, cx, 1, "studio");
    let _quick = with_quick_shell(&view, cx, &mut fake);
    assert!(matches!(state(handle, cx), (true, Some(_))));
    let mut in_panel = VisualTestContext::from_window(panel_window, cx);
    in_panel.simulate_keystrokes("escape");
    assert!(state(handle, cx).0, "Esc is the shell's");
}

/// A quick shell that ends is put away with the panel and its tile; the next toggle opens a
/// new one.
#[gpui::test]
fn an_ended_quick_shell_goes_and_the_next_show_opens_another(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let (session, quick, _shell) = with_quick_shell(&view, cx, &mut fake);
    let handle = panel(&view, cx);
    let key = fake.key;
    view.update_in(cx, |v, _window, cx| {
        let ended = SessionSummary {
            state: SessionState::Exited { status: 0 },
            ..summary(session, Some("/Users/me/src"))
        };
        v.session_opened(key, ended, cx);
    });
    cx.run_until_parked();
    assert!(!state(handle, cx).0, "put away");
    assert!(view.read_with(cx, |v, _| v.tile_of(quick.item).is_none()), "its tile is gone");
    fake.drain();
    toggle(&view, cx);
    assert_eq!(asked(&fake.drain()).len(), 1, "a new shell");
}

/// The sheet slides down from above the window; under Reduce Motion it is in place at once.
#[gpui::test]
fn the_quick_terminal_slides_in_unless_motion_is_reduced(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    toggle(&view, cx);
    let panel_window: AnyWindowHandle = panel(&view, cx).into();
    let top = |cx: &mut VisualTestContext| {
        let mut in_panel = VisualTestContext::from_window(panel_window, cx);
        in_panel.run_until_parked();
        in_panel.debug_bounds("quick-sheet").map(|b| f32::from(b.top()))
    };
    let first = top(cx).expect("drawn");
    assert!(first < 0.0, "above its place as it starts: {first}");

    toggle(&view, cx);
    cx.update(|_window, cx| cx.set_reduce_motion(true));
    toggle(&view, cx);
    assert_eq!(top(cx), Some(0.0), "in place at once");
}

/// With the panel shown but another window holding the keyboard (hide-when-unfocused off), the
/// chord brings it to the front, and the palette's command, which runs in the workspace's
/// window, puts it away. With no chord and nothing hiding it, the palette is the way to.
#[gpui::test]
fn the_palettes_command_puts_a_shown_quick_terminal_away(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let config = QuickConfig { autohide: false, keyboard: true, ..QuickConfig::default() };
    view.update_in(cx, |v, _window, cx| v.set_quick_terminal(config, cx));
    let _quick = with_quick_shell(&view, cx, &mut fake);
    let handle = panel(&view, cx);
    let panel_window: AnyWindowHandle = handle.into();
    let away = |cx: &mut VisualTestContext| {
        VisualTestContext::from_window(panel_window, cx).deactivate_window();
        cx.run_until_parked();
    };
    let run = |cx: &mut VisualTestContext, from: QuickToggle| {
        view.update_in(cx, |v, _window, cx| v.toggle_quick_terminal(Instant::now(), from, cx));
        cx.run_until_parked();
    };
    away(cx);
    assert!(state(handle, cx).0, "shown behind the workspace's window");
    run(cx, QuickToggle::Chord);
    assert!(state(handle, cx).0, "the chord brings it to the front");

    away(cx);
    run(cx, QuickToggle::Command);
    assert!(!state(handle, cx).0, "the palette's command puts it away");
    run(cx, QuickToggle::Command);
    assert!(state(handle, cx).0, "and shows it again");
}

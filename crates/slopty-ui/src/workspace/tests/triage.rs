//! The inbox worked by keyboard, the attention ladder across workers, and the word in the
//! corner for what needs the person off screen.

use std::time::Duration;

use super::*;
use crate::workspace::Finished;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn finished(command: &str, exit: u8) -> Finished {
    Finished { command: command.to_owned(), exit: Some(exit), elapsed: Duration::from_secs(40) }
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

/// The bell's inbox takes the keyboard: J walks to a row, E marks it done and goes on to the
/// next, the notice's Undo puts it back, H snoozes one out of *Unread* and the bell, and ↵ goes
/// to a row's tile and closes the inbox.
#[gpui::test]
fn the_inbox_is_worked_by_keyboard(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let studio = connect(&view, cx, 1, "studio");
    let (built, lint) = (SessionId::new(), SessionId::new());
    let built_tile = opens(&view, cx, &studio, built, studio.me, 1);
    let _lint = opens(&view, cx, &studio, lint, studio.me, 2);
    let _here = opens(&view, cx, &studio, SessionId::new(), studio.me, 3);
    view.update_in(cx, |v, _w, cx| {
        v.command_finished(built, finished("cargo build", 0), cx);
        v.command_finished(lint, finished("cargo clippy", 101), cx);
    });
    cx.run_until_parked();
    let count = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.inbox_count());
    assert_eq!(count(cx), 2);
    click(cx, "bell");

    cx.simulate_keystrokes("j");
    cx.run_until_parked();
    cx.simulate_keystrokes("e");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.finished(lint).is_none()), "the newest, marked done");
    assert_eq!(count(cx), 1);
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()).as_deref(), Some("Marked done"));
    click(cx, "toast-undo");
    assert!(view.read_with(cx, |v, _| v.finished(lint).is_some()), "and back");
    assert_eq!(count(cx), 2);

    // The selection went on to the build's row; H snoozes it out of the bell's count.
    cx.simulate_keystrokes("h");
    cx.run_until_parked();
    assert_eq!(count(cx), 1, "snoozed");
    assert!(cx.debug_bounds(leak(format!("inbox-finished-{built}"))).is_none(), "out of Unread");
    assert!(view.read_with(cx, |v, _| v.finished(built).is_some()), "its badge stays");
    view.update_in(cx, |v, _w, cx| v.command_finished(built, finished("cargo build", 0), cx));
    cx.run_until_parked();
    assert_eq!(count(cx), 2, "a new finish wakes it");

    // The build's new finish leads the list, above the lint's row the selection is on.
    cx.simulate_keystrokes("k");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("inbox").is_none(), "↵ closes the inbox");
    assert_eq!(focused(&view, cx), Some(built_tile), "and goes to the row's tile");
}

/// ⌘⇧A walks the ladder: the agent that needs you, then the failed finish, then the one that
/// ended well, and round again. A finish looked at leaves the ladder, and the walk goes on from
/// its rung rather than back to the top.
#[gpui::test]
fn the_next_attention_chord_walks_the_ladder(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(ok, ok_tile), (bad, bad_tile), (asks, asks_tile)] = three_shells(&view, cx, &studio);
    let _here = opens(&view, cx, &studio, SessionId::new(), studio.me, 4);
    view.update_in(cx, |v, _w, cx| {
        v.command_finished(ok, finished("cargo build", 0), cx);
        v.command_finished(bad, finished("cargo test", 101), cx);
        v.agent_event(blocked(asks), cx);
    });
    cx.run_until_parked();
    let mut walked = Vec::new();
    for _ in 0..4 {
        cx.simulate_keystrokes("cmd-shift-a");
        cx.run_until_parked();
        walked.extend(focused(&view, cx));
    }
    assert_eq!(walked, [asks_tile, bad_tile, ok_tile, asks_tile]);
}

/// With the app in front, an agent off screen that starts to need the person says so in the
/// corner, with "Go"; one in view says it itself.
#[gpui::test]
fn an_agent_off_screen_that_needs_you_says_so_in_the_corner(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(600.0), px(500.0)));
    let studio = connect(&view, cx, 1, "studio");
    let [(away, away_tile), _, (here, _)] = three_shells(&view, cx, &studio);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let on_screen = |cx: &mut VisualTestContext, t: TileRef| {
        view.read_with(cx, |v, _| v.drawn.on_screen.borrow().contains(&t.item))
    };
    assert!(!on_screen(cx, away_tile), "the first column is out of view");

    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(here), cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()), None, "in view it says it itself");

    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(away), cx));
    cx.run_until_parked();
    let said = view.read_with(cx, |v, _| v.toast_text()).expect("a word in the corner");
    assert!(said.ends_with("has a question"), "{said}");
    click(cx, "toast-go");
    assert_eq!(focused(&view, cx), Some(away_tile));
}

/// What the keyboard opens arrives whole, with no fade: the palette and the inbox by their
/// keys. The bell's click and the "…" menu's palette row keep the fade.
#[gpui::test]
fn what_a_key_opens_arrives_without_a_fade(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    let fades = view.read_with(cx, |v, cx| v.palette.as_ref().map(|p| p.read(cx).fades_in()));
    assert_eq!(fades, Some(false), "the palette, by its key, arrives whole");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-shift-u");
    cx.run_until_parked();
    assert!(cx.debug_bounds("inbox").is_some(), "⌘⇧U opens the inbox");
    assert!(view.read_with(cx, |v, _| v.menu_keyed), "whole in its first frame");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("inbox").is_none());
    click(cx, "bell");
    assert!(cx.debug_bounds("inbox").is_some());
    assert!(!view.read_with(cx, |v, _| v.menu_keyed), "the pointer's open keeps its fade");
    click(cx, "bell");

    click(cx, "more");
    click(cx, "menu-Command palette");
    let fades = view.read_with(cx, |v, cx| v.palette.as_ref().map(|p| p.read(cx).fades_in()));
    assert_eq!(fades, Some(true), "the palette, by the pointer, fades in");
}

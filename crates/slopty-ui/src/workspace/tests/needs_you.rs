//! The bell and what it opens, the attention ladder across workers, and the word in the corner
//! for what needs the person off screen.

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

/// Type `text` into the navigator's filter.
fn filter(cx: &mut VisualTestContext, text: &str) {
    click(cx, "nav-filter");
    cx.simulate_input(text);
    cx.run_until_parked();
}

/// The bell counts the agents that need the person and the turns left to review, never a
/// shell's finish, which is its tile's dot alone and only past the slow-command time. Clicked,
/// or by ⌘⇧U, it shows a hidden navigator at *Needs you*, emptying a filter that would hide it.
#[gpui::test]
fn the_bell_counts_what_needs_you_and_shows_the_navigator_at_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let studio = connect(&view, cx, 1, "studio");
    let [(quick, _), (built, _), (asks, _)] = three_shells(&view, cx, &studio);
    let _here = opens(&view, cx, &studio, SessionId::new(), studio.me, 4);
    let mut short = finished("cargo check", 0);
    short.elapsed = Duration::from_secs(20);
    view.update_in(cx, |v, _w, cx| {
        v.command_finished(quick, short, cx);
        v.command_finished(built, finished("cargo build", 0), cx);
    });
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.finished(quick).is_none()), "20 s is not slow");
    assert!(view.read_with(cx, |v, _| v.finished(built).is_some()), "40 s marks its tile");
    let count = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.bell_count());
    assert_eq!(count(cx), 0, "a shell's finish is not the bell's");
    assert!(cx.debug_bounds("bell-count").is_none());
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()), None, "nor the corner's");

    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(asks), cx));
    cx.run_until_parked();
    assert_eq!(count(cx), 1);
    let waiting = leak(format!("nav-waiting-{asks}"));
    assert!(cx.debug_bounds(waiting).is_some(), "listed though its tile's row is in view");

    filter(cx, "zzz");
    assert!(cx.debug_bounds(waiting).is_none(), "the filter hides it");
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_none(), "hidden");
    click(cx, "bell");
    assert!(cx.debug_bounds("navigator").is_some(), "the bell shows it");
    assert_eq!(view.read_with(cx, |v, _| v.navigator_filter().to_owned()), "");
    assert!(cx.debug_bounds("nav-needs-you").is_some() && cx.debug_bounds(waiting).is_some());

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_none());
    cx.simulate_keystrokes("cmd-shift-u");
    cx.run_until_parked();
    assert!(cx.debug_bounds(waiting).is_some(), "and so does its key");
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

/// With the app in front, an agent off screen that comes to need the person says so in the
/// corner, with "Go" alone; one in view says it itself. A failure or a finish off screen says
/// nothing there: its tile's mark holds it. A word whose agent was answered anywhere goes at
/// once.
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
    let said = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.toast_text());

    view.update_in(cx, |v, _w, cx| {
        v.command_finished(away, finished("cargo test", 101), cx);
        v.agent_event(blocked(here), cx);
    });
    cx.run_until_parked();
    assert_eq!(said(cx), None, "a failure off screen, and an agent in view, say nothing here");

    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(away), cx));
    cx.run_until_parked();
    let line = said(cx).expect("a word in the corner");
    assert!(line.ends_with("has a question"), "{line}");
    assert!(cx.debug_bounds("toast-go").is_some(), "Go");
    assert!(cx.debug_bounds("toast-allow").is_none() && cx.debug_bounds("toast-deny").is_none());

    // Answered elsewhere: the word goes at once.
    let mut working = blocked(away);
    working.status = AgentStatus::Working;
    view.update_in(cx, |v, _w, cx| v.agent_event(working, cx));
    cx.run_until_parked();
    assert_eq!(said(cx), None, "answered, the pointer goes");

    let mut again = blocked(away);
    again.status = AgentStatus::Working;
    view.update_in(cx, |v, _w, cx| v.agent_event(again, cx));
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(away), cx));
    cx.run_until_parked();
    assert!(said(cx).is_some());
    click(cx, "toast-go");
    assert_eq!(focused(&view, cx), Some(away_tile));
}

/// What the keyboard opens arrives whole, with no fade: the palette by its key. The "…" menu's
/// palette row keeps the fade.
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

    click(cx, "more");
    click(cx, "menu-Command palette");
    let fades = view.read_with(cx, |v, cx| v.palette.as_ref().map(|p| p.read(cx).fades_in()));
    assert_eq!(fades, Some(true), "the palette, by the pointer, fades in");
}

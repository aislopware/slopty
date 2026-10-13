//! The bell and what it opens, the attention ladder across workers, and the word in the corner
//! for what needs the person off screen.

use std::time::Duration;

use super::*;
use crate::workspace::Finished;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn finished(command: &str, exit: u8) -> Finished {
    Finished {
        command: command.to_owned(),
        exit: Some(exit),
        elapsed: Duration::from_secs(40),
        turn: None,
    }
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

/// Type `text` into the navigator's filter, shown by "Search" where it is hidden.
fn filter(cx: &mut VisualTestContext, text: &str) {
    let shown = cx.debug_bounds("nav-filter").is_some();
    click(cx, if shown { "nav-filter" } else { "nav-search" });
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

    // A phone's "…" opens it by the pointer.
    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    click(cx, "more");
    click(cx, "menu-Command palette");
    let fades = view.read_with(cx, |v, cx| v.palette.as_ref().map(|p| p.read(cx).fades_in()));
    assert_eq!(fades, Some(true), "the palette, by the pointer, fades in");
}

/// A *Needs you* row is one line, its words the ask, and it acts through icon buttons in its
/// end slot (`.research/design-polish-2026-10-12.md` §4.5): at rest the end says how long it
/// has waited and no button stands; under the pointer Deny (`x` in `error`) and Allow (`check`
/// in `success`) take that place, 22 pt squares, the yes last, inside the row; Allow answers.
#[gpui::test]
fn the_actions_show_on_hover_as_buttons(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::Intent;
    use slopty_proto::thread::{AskId, Choice, Effect};
    let choice = |id: &str, effect| Choice {
        id: id.to_owned(),
        label: id.to_owned(),
        effect,
        scope: None,
        stops: false,
    };
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let mut row = thread_waits::asking(None);
    let thread = row.id;
    if let Some(card) = row.requests.first_mut() {
        card.options = vec![choice("accept", Effect::Allow), choice("decline", Effect::Deny)];
    }
    thread_waits::table(&view, cx, key, vec![row]);
    cx.simulate_mouse_move(point(px(1.0), px(1.0)), None, Modifiers::default());
    cx.run_until_parked();
    let row = cx.debug_bounds(leak(format!("nav-waiting-{thread}"))).expect("its row");
    let theme = Theme::default();
    assert!((f32::from(row.size.height) - theme.density.row).abs() < 0.5, "one line: {row:?}");
    let words = cx.debug_bounds(leak(format!("nav-waiting-words-{thread}"))).expect("the ask");
    assert!(row.contains(&words.center()), "on its line");
    let (allow, deny) = (leak(format!("nav-allow-{thread}")), leak(format!("nav-deny-{thread}")));
    assert!(cx.debug_bounds(allow).is_none() && cx.debug_bounds(deny).is_none(), "at rest");

    cx.simulate_mouse_move(row.center(), None, Modifiers::default());
    cx.run_until_parked();
    let (allow_at, deny_at) = (
        cx.debug_bounds(allow).expect("Allow under the pointer"),
        cx.debug_bounds(deny).expect("Deny under the pointer"),
    );
    let side = slopty_kit::kit::icon_button_side(&theme);
    for button in [allow_at, deny_at] {
        assert!((f32::from(button.size.width) - side).abs() < 0.5, "an icon button: {button:?}");
        assert!(row.contains(&button.center()), "inside the row: {button:?} {row:?}");
    }
    assert!(deny_at.right() <= allow_at.left(), "the yes last: {deny_at:?} {allow_at:?}");
    let nodes = tree(cx);
    for name in [crate::workspace::approvals::ALLOW, crate::workspace::approvals::DENY] {
        assert!(nodes.iter().any(|n| n.is("Button", Some(name))), "{name}: {nodes:#?}");
    }
    studio.drain();
    cx.simulate_click(allow_at.center(), Modifiers::default());
    cx.run_until_parked();
    let sent: Vec<Intent> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(slopty_proto::thread::wire::ThreadRequest::Intent {
                intent, ..
            }) => Some(intent),
            _ => None,
        })
        .collect();
    let accepted = Intent::Answer {
        ask: AskId("ask-1".to_owned()),
        choice: "accept".to_owned(),
        message: None,
    };
    assert_eq!(sent, [accepted], "Allow answers, once");
}

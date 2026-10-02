//! A thread driven over a protocol (Codex, pi, an ACP agent) has no terminal whose agent status
//! could say that it waits: its row in its worker's table says it to the chrome instead, and a
//! thread whose terminal's agent already says it is not counted twice.

use slopty_proto::thread::wire::{RequestCard, TableFrame, ThreadRow};
use slopty_proto::thread::{AskId, Cursor, Request};

use super::*;
use crate::icons::Status;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

/// A thread on `terminal`, or on none, waiting on the person's yes or no.
fn asking(terminal: Option<SessionId>) -> ThreadRow {
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = terminal;
    let mut row = state.row(WallMs::ZERO);
    row.requests = vec![RequestCard {
        id: AskId("ask-1".to_owned()),
        item: None,
        kind: Request::APPROVAL.to_owned(),
        title: "Run `cargo test`".to_owned(),
        options: Vec::new(),
        opened_ms: WallMs::ZERO,
    }];
    row
}

fn table(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    key: WorkerKey,
    rows: Vec<ThreadRow>,
) {
    let table = TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table, cx));
    cx.run_until_parked();
}

/// The status bar's agents line, as a screen reader hears it.
fn agents_said(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    cx.update(|window, _cx| crate::a11y::tree(window))
        .into_iter()
        .find(|n| n.role == "Status" && n.label.as_deref().is_some_and(|l| l.contains("blocked")))
        .and_then(|n| n.label)
}

/// A Codex thread waiting on an approval marks its navigator row, wears the header's pill,
/// counts on the bell, the Dock and the status bar, lists in the inbox with what it asks, and
/// its row there opens its tile. Answered, every one of them lets go.
#[gpui::test]
fn a_thread_with_no_terminal_that_waits_says_so_across_the_chrome(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let row = asking(None);
    let thread = row.id;
    let tile = arrives(&view, cx, &studio, ItemKind::Thread { thread }, 1);
    let file = arrives(&view, cx, &studio, ItemKind::File { path: "/w/a.md".to_owned() }, 2);
    table(&view, cx, key, vec![row.clone()]);

    view.update(cx, |v, _| {
        let item = v.item(tile).cloned().expect("the tile");
        assert_eq!(v.tile_status(tile, &item), Some(Status::NeedsYou), "the navigator's glyph");
        assert_eq!((v.needs_you_count(), v.needs_you_on(key)), (1, 1), "the Dock's count");
        assert_eq!(v.inbox_count(), 1, "the bell's");
        let look = v.attention_look();
        let [asks] = look.asking.as_slice() else { panic!("one asks: {look:?}") };
        assert_eq!(asks.route.about, attention::About::Thread(thread), "a note of its own");
        assert_eq!(asks.route.item, Some(tile.item), "that leads to its tile");
        assert_eq!(asks.body, "Run cargo test", "what it asks, said plainly");
    });
    let pill = leak(format!("agent-{}", tile.item.as_uuid()));
    assert!(cx.debug_bounds(pill).is_some(), "the header's pill");
    assert_eq!(agents_said(&view, cx).as_deref(), Some("1 blocked"), "the status bar");

    cx.update(|_w, cx| cx.set_reduce_motion(true));
    let bell = cx.debug_bounds("bell").expect("the bell");
    cx.simulate_click(bell.center(), Modifiers::none());
    cx.run_until_parked();
    let line = leak(format!("inbox-thread-{thread}"));
    let at = cx.debug_bounds(line).expect("an inbox row of its own");
    view.update_in(cx, |v, _w, cx| v.focus_tile(file, cx));
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(file));
    cx.simulate_click(at.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(tile), "its row goes to its tile");

    let mut answered = row;
    answered.requests.clear();
    answered.status.phase = slopty_proto::thread::Phase::Working;
    table(&view, cx, key, vec![answered]);
    view.update(cx, |v, _| {
        let item = v.item(tile).cloned().expect("the tile");
        assert_eq!(v.tile_status(tile, &item), Some(Status::Working));
        assert_eq!((v.needs_you_count(), v.inbox_count()), (0, 0), "answered, it lets go");
    });
}

/// A thread whose terminal's agent already says it waits (Claude Code, heard through its
/// hooks) is counted once, by its terminal.
#[gpui::test]
fn a_thread_its_terminal_speaks_for_is_counted_once(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let session = SessionId::new();
    let _shell = opens(&view, cx, &studio, session, studio.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(blocked(session), cx);
        v.threads_linked(key, cx);
    });
    table(&view, cx, key, vec![asking(Some(session))]);
    view.update(cx, |v, _| {
        assert_eq!(v.needs_you_count(), 1, "once");
        assert_eq!(v.attention_look().asking.len(), 1);
    });
}

//! The steps of a thread beyond its words: a subagent's thread opened from its call and left
//! again, every step opened at once, the commands run in the background, and a message's
//! copy.

use gpui::{Modifiers, TestAppContext};
use slopty_core::WallMs;
use slopty_proto::ClientMsg;
use slopty_proto::thread::detail::{ExecDetail, ExecStatus};
use slopty_proto::thread::wire::{TableFrame, ThreadRequest};
use slopty_proto::thread::{
    BackgroundTask, Changed, Clipped, Cursor, Item, ItemBody, ItemId, Link, ThreadId, ToolCall,
    ToolDetail, ToolState, Turn, TurnId, TurnState, Usage, UserMessage, kind,
};

use super::{asked, hub, snapshot, view};
use crate::conversation::CycleDensity;
use crate::conversation::thread::fixtures;
use crate::conversation::thread::hub::ThreadHub;
use crate::conversation::thread::rows::Row;
use crate::conversation::thread::view::ThreadView;

fn turn(id: u32, state: TurnState) -> Turn {
    let ended_ms = (!matches!(state, TurnState::Active)).then(|| WallMs::from_millis(5_000));
    Turn {
        id: TurnId(id),
        input: None,
        state,
        started_ms: WallMs::from_millis(1_000),
        ended_ms,
        usage: Usage::default(),
        models: Vec::new(),
        changed: Changed::default(),
        before: None,
        after: None,
    }
}

fn item(id: &str, turn: u32, body: ItemBody) -> Item {
    Item { id: ItemId(id.to_owned()), turn: TurnId(turn), at_ms: WallMs::ZERO, body }
}

fn user(id: &str, turn: u32) -> Item {
    item(
        id,
        turn,
        ItemBody::User(UserMessage {
            text: Clipped::whole("Count the lines"),
            images: Vec::new(),
            command: None,
            intent: None,
        }),
    )
}

fn call(
    id: &str,
    turn: u32,
    kind: &str,
    detail: Option<ToolDetail>,
    child: Option<ThreadId>,
) -> Item {
    item(
        id,
        turn,
        ItemBody::Tool(Box::new(ToolCall {
            name: kind.to_owned(),
            kind: kind.to_owned(),
            title: "Count lines".to_owned(),
            input: Clipped::default(),
            state: ToolState::Completed,
            output: Some(Clipped::whole("Compiling a\nCompiling b\n")),
            images: Vec::new(),
            detail,
            child,
            ended_ms: None,
        })),
    )
}

fn background(status: ExecStatus) -> ToolDetail {
    ToolDetail::Exec(ExecDetail {
        command: Clipped::whole("cargo build"),
        description: Some("Build it".to_owned()),
        cwd: None,
        background: true,
        task: Some("b1".to_owned()),
        status,
        exit_code: None,
        stderr: None,
        duration_ms: None,
    })
}

fn follows(sent: &super::Sent) -> Vec<ThreadRequest> {
    sent.borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(
                r @ (ThreadRequest::Follow { .. } | ThreadRequest::Unfollow { .. }),
            ) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

/// A subagent's call opens its thread in the view, followed for it, under a bar that leads
/// back; the tile's own thread stays the view's, and Esc goes back and lets the subagent's go.
#[gpui::test]
fn a_subagent_s_call_opens_its_thread_and_esc_leads_back(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut parent = fixtures::empty();
    let mut child = fixtures::empty();
    child.meta.title = "Count lines".to_owned();
    child.meta.parent = Some(Link { thread: parent.meta.id, item: ItemId("agent".to_owned()) });
    let (main, sub) = (parent.meta.id, child.meta.id);
    parent.turns = vec![turn(1, TurnState::Active)];
    parent.items = vec![user("u", 1), call("agent", 1, kind::AGENT, None, Some(sub))];
    let rows = vec![parent.row(WallMs::ZERO), child.row(WallMs::ZERO)];
    hub.update(cx, ThreadHub::connected);
    hub.update(cx, |hub, cx| {
        hub.table(&TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows }, cx);
    });
    let (view, cx) = view(cx, &hub, main);
    hub.update(cx, |hub, cx| hub.frame(main, snapshot(parent, 2), cx));
    cx.run_until_parked();
    sent.borrow_mut().clear();

    let at = cx.debug_bounds("tool-agent").expect("the subagent's call").center();
    cx.simulate_click(at, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.shown()), sub, "its thread on show");
    assert_eq!(view.read_with(cx, |v, _| v.thread()), main, "still the tile's thread view");
    assert!(cx.debug_bounds("thread-trail").is_some(), "the bar that leads back");
    assert!(cx.debug_bounds("thread-composer").is_none(), "a subagent takes no messages");
    assert!(
        follows(&sent)
            .iter()
            .any(|r| matches!(r, ThreadRequest::Follow { thread, .. } if *thread == sub)),
        "followed for it: {:?}",
        follows(&sent)
    );
    hub.update(cx, |hub, cx| hub.frame(sub, snapshot(child, 1), cx));
    cx.run_until_parked();

    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |v, _| v.shown()), main, "back");
    assert!(cx.debug_bounds("thread-trail").is_none());
    assert!(follows(&sent).contains(&ThreadRequest::Unfollow { thread: sub }), "let go");
    assert!(cx.debug_bounds("tool-agent").is_some(), "drawn at once");
}

/// A line of the work in the background opens what it is: a subagent left running opens its
/// thread, as its call's card does; a command shows what it printed under its line, and a
/// second press folds it. Work with nothing to show is a line alone.
#[gpui::test]
fn a_background_task_opens_from_its_line(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut parent = fixtures::empty();
    let mut child = fixtures::empty();
    child.meta.parent = Some(Link { thread: parent.meta.id, item: ItemId("agent".to_owned()) });
    let (main, sub) = (parent.meta.id, child.meta.id);
    parent.turns = vec![turn(1, TurnState::Active)];
    parent.items = vec![user("u", 1), call("agent", 1, kind::AGENT, None, Some(sub))];
    let task = |id: &str, kind: &str, item: Option<&str>, output: Option<&str>| BackgroundTask {
        id: id.to_owned(),
        kind: kind.to_owned(),
        title: format!("task {id}"),
        state: BackgroundTask::RUNNING.to_owned(),
        item: item.map(|i| ItemId(i.to_owned())),
        output: output.map(Clipped::whole),
        started_ms: WallMs::from_millis(1_000),
        ended_ms: None,
    };
    parent.tasks = vec![
        task("b1", BackgroundTask::SHELL, None, Some("Compiling a\nFinished dev\n")),
        task("a1", BackgroundTask::AGENT, Some("agent"), None),
        task("q1", BackgroundTask::SHELL, None, None),
    ];
    let rows = vec![parent.row(WallMs::ZERO), child.row(WallMs::ZERO)];
    hub.update(cx, ThreadHub::connected);
    hub.update(cx, |hub, cx| {
        hub.table(&TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows }, cx);
    });
    let (view, cx) = view(cx, &hub, main);
    hub.update(cx, |hub, cx| hub.frame(main, snapshot(parent, 2), cx));
    cx.run_until_parked();
    let click = |cx: &mut gpui::VisualTestContext, what: &'static str| {
        let at = cx.debug_bounds(what).unwrap_or_else(|| panic!("{what} is drawn")).center();
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    };
    click(cx, "thread-tasks");

    assert!(cx.debug_bounds("task-output-b1").is_none(), "folded until asked");
    click(cx, "task-b1");
    let output = cx.debug_bounds("task-output-b1").expect("what it printed, under its line");
    let line = cx.debug_bounds("task-b1").expect("its line");
    assert!(output.top() > line.top() && output.bottom() <= line.bottom(), "inside the line");
    click(cx, "task-b1");
    assert!(cx.debug_bounds("task-output-b1").is_none(), "a second press folds it");

    click(cx, "task-q1");
    assert!(cx.debug_bounds("task-output-q1").is_none(), "nothing printed, nothing to open");
    assert_eq!(view.read_with(cx, |v, _| v.shown()), main);

    click(cx, "task-a1");
    assert_eq!(view.read_with(cx, |v, _| v.shown()), sub, "the subagent's thread on show");
    assert!(cx.debug_bounds("thread-trail").is_some(), "with the way back");
}

/// ⌃O opens every settled turn and each of its steps; again, they fold.
#[gpui::test]
fn control_o_opens_every_step_and_folds_them_again(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Complete), turn(2, TurnState::Complete)];
    state.items = vec![
        user("u1", 1),
        call("c1", 1, kind::READ, None, None),
        user("u2", 2),
        call("c2", 2, kind::READ, None, None),
    ];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    let tools = |view: &gpui::Entity<ThreadView>, cx: &mut gpui::VisualTestContext| {
        view.read_with(cx, |v, _| v.rows().iter().filter(|r| matches!(r, Row::Tool { .. })).count())
    };
    assert_eq!(tools(&view, cx), 0, "folded");
    cx.dispatch_action(CycleDensity);
    assert_eq!(tools(&view, cx), 2, "every step");
    cx.dispatch_action(CycleDensity);
    assert_eq!(tools(&view, cx), 0, "folded again");
}

/// A command run in the background shows over the composer with its last line while it runs,
/// and once it ended only for the turn it ended in.
#[gpui::test]
fn a_background_command_shows_while_it_runs(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Complete), turn(2, TurnState::Active)];
    state.items = vec![
        user("u1", 1),
        call("old", 1, kind::EXEC, Some(background(ExecStatus::Done)), None),
        user("u2", 2),
        call("build", 2, kind::EXEC, Some(background(ExecStatus::Running)), None),
    ];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("background-build").is_some(), "the running one");
    assert!(cx.debug_bounds("background-old").is_none(), "not one an earlier turn ended");
}

/// A message's copy puts its words on the clipboard.
#[gpui::test]
fn a_message_s_copy_puts_its_words_on_the_clipboard(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Complete)];
    state.items = vec![user("u", 1)];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("copy-u").is_none(), "quiet until the pointer is on it");
    let message = cx.debug_bounds("item-u").expect("the message").center();
    cx.simulate_mouse_move(message, None, Modifiers::none());
    cx.run_until_parked();
    let copy = cx.debug_bounds("copy-u").expect("its copy").center();
    cx.simulate_click(copy, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(cx.read_from_clipboard().and_then(|c| c.text()).as_deref(), Some("Count the lines"));
}

/// Two turns, each a question and its answer, at `width`.
fn two_turns(
    cx: &mut TestAppContext,
    width: f32,
) -> (gpui::Entity<ThreadView>, &mut gpui::VisualTestContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Complete), turn(2, TurnState::Complete)];
    state.items = vec![
        user("u1", 1),
        item("a1", 1, ItemBody::Text(Clipped::whole("Twelve."))),
        user("u2", 2),
        item("a2", 2, ItemBody::Text(Clipped::whole("Done."))),
    ];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(600.0)));
    view.update(cx, |v, cx| v.set_layout(width, cx));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    (view, cx)
}

/// Turns group: an answer follows its question at the small step and the next question opens
/// at the large one, as the person's actions wait beside the bubble's foot instead of keeping a
/// hidden line under it.
#[gpui::test]
fn a_question_and_its_answer_read_as_one_turn(cx: &mut TestAppContext) {
    let (_view, cx) = two_turns(cx, 800.0);
    let spacing = slopty_theme::Theme::default().spacing;
    let bounds = |cx: &mut gpui::VisualTestContext, id: &'static str| {
        cx.debug_bounds(id).unwrap_or_else(|| panic!("{id} drawn"))
    };
    let (u1, a1, u2) = (bounds(cx, "item-u1"), bounds(cx, "item-a1"), bounds(cx, "item-u2"));
    let within = f32::from(a1.top() - u1.bottom());
    let between = f32::from(u2.top() - a1.bottom());
    assert!((within - spacing.sm).abs() < 0.5, "the answer at the small step: {within}");
    assert!(between >= spacing.lg - 0.5, "the next question at the large one: {between}");

    cx.simulate_mouse_move(u1.center(), None, Modifiers::none());
    cx.run_until_parked();
    let copy = bounds(cx, "copy-u1");
    assert!(copy.top() >= u1.top() && copy.bottom() <= u1.bottom(), "beside the bubble {copy:?}");
}

/// The column stands off the tile by its room's gutter: wide, regular and narrow.
#[gpui::test]
fn the_column_s_gutter_follows_the_tile_s_room(cx: &mut TestAppContext) {
    let spacing = slopty_theme::Theme::default().spacing;
    let (view, cx) = two_turns(cx, 800.0);
    for (width, gutter) in [(800.0, spacing.xxxl), (600.0, spacing.xl), (360.0, spacing.lg)] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(600.0)));
        view.update(cx, |v, cx| v.set_layout(width, cx));
        cx.run_until_parked();
        let answer = cx.debug_bounds("item-a1").expect("the answer");
        let left = f32::from(answer.left());
        assert!((left - gutter).abs() < 0.5, "{width}: the gutter {gutter}, at {left}");
    }
}

/// A message the person sent into a running turn stands between two folds, each saying what
/// its stretch of the work did; the turn's time stands on the last. Either opens the turn.
#[gpui::test]
fn a_steer_stands_between_the_folds_of_its_turn(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Complete)];
    state.items = vec![
        user("u", 1),
        call("x", 1, kind::EXEC, None, None),
        user("s", 1),
        call("r", 1, kind::READ, None, None),
        item("a", 1, ItemBody::Text(Clipped::whole("Done."))),
    ];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    for drawn in ["fold-1-0", "item-s", "fold-1"] {
        assert!(cx.debug_bounds(drawn).is_some(), "{drawn}");
    }
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Button", Some("Ran a command"))), "{tree:#?}");
    assert!(tree.iter().any(|n| n.is("Button", Some("Worked 4 s: Read a file"))), "{tree:#?}");
    let at = cx.debug_bounds("fold-1-0").expect("the first fold").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    let tools = view
        .read_with(cx, |v, _| v.rows().iter().filter(|r| matches!(r, Row::Tool { .. })).count());
    assert_eq!(tools, 2, "the turn opens as one");
}

/// A turn's work is one line: open while the turn works, its calls under it as they come and
/// the line saying what they did so far; folded by a click; folded once the turn is done,
/// saying how long it took.
#[gpui::test]
fn a_turns_work_is_open_under_its_line_while_it_runs_and_folds_when_done(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Active)];
    state.status.phase = slopty_proto::thread::Phase::Working;
    state.items = vec![user("u", 1), call("x", 1, kind::EXEC, None, None)];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let line = tree.iter().any(|n| n.is("Button", Some("Working: Ran a command")));
    assert!(line, "what it did so far: {tree:#?}");
    assert!(cx.debug_bounds("tool-x").is_some(), "its call under it");
    assert!(cx.debug_bounds("thread-working").is_some(), "and it says it works");

    let at = cx.debug_bounds("fold-1").expect("the line").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("tool-x").is_none(), "folded by the reader");
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("tool-x").is_some(), "and opened again");

    state.turns = vec![turn(1, TurnState::Complete)];
    state.status.phase = slopty_proto::thread::Phase::Idle;
    state.items.push(item("a", 1, ItemBody::Text(Clipped::whole("Done."))));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("tool-x").is_none(), "folded once the turn is done");
    assert!(cx.debug_bounds("item-a").is_some(), "over its answer");
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let line = tree.iter().any(|n| n.is("Button", Some("Worked 4 s: Ran a command")));
    assert!(line, "{tree:#?}");
    let rows = view.read_with(cx, |v, _| v.rows().len());
    assert_eq!(rows, 3, "the message, the line and the answer");
}

/// A call on one file is one line: the file's type, its verb and the file named first, and the
/// name opens the file on the thread's machine, under the agent's folder, while the rest of the
/// line opens the call. A failed call is marked at its end with an icon and a word.
#[gpui::test]
fn a_call_names_its_file_which_opens_and_a_failure_is_marked_at_its_end(cx: &mut TestAppContext) {
    use slopty_proto::thread::detail::ReadDetail;

    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Active)];
    state.status.phase = slopty_proto::thread::Phase::Working;
    let read = ToolDetail::Read(ReadDetail {
        path: "src/lib.rs".to_owned(),
        offset: None,
        limit: None,
        lines: None,
        total_lines: None,
    });
    let mut failed = call("f", 1, kind::EXEC, None, None);
    if let ItemBody::Tool(call) = &mut failed.body {
        call.state = ToolState::Failed;
    }
    state.items = vec![user("u", 1), call("r", 1, kind::READ, Some(read), None), failed];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    let asked = asked(cx, &view);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Link", Some("Open /w/src/lib.rs"))), "{tree:#?}");
    assert!(tree.iter().any(|n| n.is("Button", Some("Read src/lib.rs"))), "{tree:#?}");
    assert!(tree.iter().any(|n| n.is("Button", Some("Count lines, Failed"))), "{tree:#?}");

    let before = view.read_with(cx, |v, _| v.keys().to_vec());
    let name = cx.debug_bounds("call-file-r").expect("the file's name").center();
    cx.simulate_click(name, Modifiers::none());
    cx.run_until_parked();
    let opened: Vec<String> = asked
        .borrow()
        .iter()
        .filter_map(|e| match e {
            crate::conversation::thread::ThreadViewEvent::OpenFile { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(opened, ["/w/src/lib.rs"], "under the agent's folder");
    let after = view.read_with(cx, |v, _| v.keys().to_vec());
    assert_eq!(before, after, "the name opened the file, not the call");
}

/// The latest turn ends in the files it changed: a card under its answer, its count and
/// lines, and a way to the review. With the turn's review come, Keep takes each file into what
/// the person has kept, as the review showed it, and the card goes.
#[gpui::test]
fn the_latest_turn_ends_in_its_changed_files_which_keep_from_there(cx: &mut TestAppContext) {
    use slopty_proto::thread::detail::EditDetail;
    use slopty_proto::thread::wire::{FileDiff, Intent, Pick, Review, ReviewScope, ThreadFrame};
    use slopty_proto::thread::{Cap, Patch};

    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.caps.push(Cap::named(Cap::SNAPSHOTS));
    state.turns = vec![turn(1, TurnState::Complete)];
    let patch = Patch { added: 2, removed: 1, ..Patch::default() };
    let edit = ToolDetail::Edit(EditDetail {
        path: "/w/src/a.rs".to_owned(),
        edits: 1,
        replace_all: false,
        patch: patch.clone(),
    });
    state.items = vec![
        user("u", 1),
        call("e", 1, kind::EDIT, Some(edit), None),
        item("a", 1, ItemBody::Text(Clipped::whole("Done."))),
    ];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let card = "Changed 1 file, 2 added, 1 removed";
    assert!(tree.iter().any(|n| n.is("Group", Some(card))), "{tree:#?}");
    assert!(tree.iter().any(|n| n.is("Button", Some("Review src/a.rs"))), "{tree:#?}");
    assert!(cx.debug_bounds("changes-keep").is_none(), "no Keep before the turn's review");
    let asked: Vec<ReviewScope> = sent
        .borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Review { scope, .. }) => Some(scope.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(asked, [ReviewScope::Turn(TurnId(1))], "the turn's own review, once");

    let review = Review {
        scope: ReviewScope::Turn(TurnId(1)),
        from: None,
        to: None,
        files: vec![FileDiff {
            path: "src/a.rs".to_owned(),
            from: Some("old".to_owned()),
            to: Some("new".to_owned()),
            kind: slopty_proto::thread::wire::FileKind::Text,
            old_path: None,
            modes: None,
            patch,
        }],
        absent: None,
    };
    hub.update(cx, |hub, cx| hub.frame(thread, ThreadFrame::Review(Box::new(review)), cx));
    cx.run_until_parked();
    let keep = cx.debug_bounds("changes-keep").expect("Keep, once the review came").center();
    assert!(cx.debug_bounds("changes-undo").is_some(), "and Undo");
    cx.simulate_click(keep, Modifiers::none());
    cx.run_until_parked();
    let kept: Vec<Intent> = super::intents(&sent);
    let pick = Pick {
        path: "src/a.rs".to_owned(),
        from: Some("old".to_owned()),
        stamp: Some("new".to_owned()),
        hunks: Vec::new(),
        old_path: None,
    };
    assert_eq!(kept, [Intent::Keep(pick)], "each file as the review showed it");
    let rows = view.read_with(cx, |v, _| v.rows().to_vec());
    assert!(!rows.iter().any(|r| matches!(r, Row::Changes { .. })), "the card goes: {rows:?}");
}

/// The prompt outline stands at the transcript's right edge in a wide tile, a bar per prompt,
/// the one in view lit; the pointer on a bar shows the prompt beside it, and a press takes the
/// transcript to it. A tile under 928 points has none.
#[gpui::test]
fn the_prompt_outline_takes_the_transcript_to_a_prompt(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let state = fixtures::long(6, 2);
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-outline").is_none(), "an 800 pt tile has none");

    cx.simulate_resize(gpui::size(gpui::px(1100.0), gpui::px(700.0)));
    view.update(cx, |v, cx| v.set_layout(1100.0, cx));
    cx.run_until_parked();
    let rail = cx.debug_bounds("thread-outline").expect("the outline in a wide tile");
    assert!(f32::from(rail.right()) > 1050.0, "at the right edge: {rail:?}");
    let first = cx.debug_bounds("outline-0").expect("a bar per prompt");
    let last = cx.debug_bounds("outline-5").expect("the sixth");
    let middle = (first.top() + last.bottom()) / 2.0;
    let rows = cx.debug_bounds("thread-rows").expect("the rows");
    let centre = (rows.top() + rows.bottom()) / 2.0;
    assert!((f32::from(middle) - f32::from(centre)).abs() < 4.0, "centred: {middle:?} {centre:?}");

    cx.simulate_mouse_move(first.center(), None, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-outline-preview").is_some(), "the prompt shows beside it");
    cx.simulate_click(first.center(), Modifiers::none());
    let prompt = view.read_with(cx, |v, _| v.prompt_rows()[0]);
    assert_eq!(view.read_with(cx, |v, _| v.top_row()), prompt, "taken to the first prompt");
}

/// A thread view drawn as a workspace tile draws it, from its cached drawing (built again only
/// when it was told something moved), above a soft keyboard `keyboard` points tall.
struct Tiled {
    view: gpui::Entity<ThreadView>,
    keyboard: f32,
}

impl gpui::Render for Tiled {
    fn render(
        &mut self,
        _w: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{ParentElement as _, Styled as _};
        let tile = gpui::StyleRefinement::default().flex_1().min_h_0().w_full();
        gpui::div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.view.clone().cached(tile))
            .child(gpui::div().flex_none().w_full().h(gpui::px(self.keyboard)))
    }
}

/// When the soft keyboard rises and shrinks the transcript under the outline, though nothing
/// tells the thread, the stack is sized to the column it now stands in by the next frame: the
/// frame shown is the one a frame from scratch draws, and every bar stays inside the rows.
#[gpui::test]
fn the_prompt_outline_follows_a_shrinking_transcript(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (hub, _sent) = hub(cx, None);
    let state = fixtures::long(40, 2);
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let hosted = hub.clone();
    let (tiled, cx) = cx.add_window_view(|window, cx| {
        let theme = slopty_theme::Theme::default();
        let view =
            gpui::AppContext::new(cx, |cx| ThreadView::new(hosted, thread, theme, window, cx));
        Tiled { view, keyboard: 0.0 }
    });
    let view = tiled.read_with(cx, |t, _| t.view.clone());
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.simulate_resize(gpui::size(gpui::px(1100.0), gpui::px(700.0)));
    view.update(cx, |v, cx| v.set_layout(1100.0, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-outline").is_some(), "the outline in a wide tile");

    for keyboard in [440.0, 549.0, 0.0, 549.0] {
        tiled.update(cx, |t, cx| {
            t.keyboard = keyboard;
            cx.notify();
        });
        cx.run_until_parked();
        // The frame the display asks for next, as the window draws it: only what was told is
        // built again.
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let stale = cx.update(|window, cx| crate::retained::stale(window, cx, 12));
        assert!(stale.is_none(), "{keyboard} pt of keyboard: {stale:?}");
        let rail = cx.debug_bounds("thread-outline").expect("the outline");
        let bars: Vec<_> =
            (0..40).filter_map(|at| cx.debug_bounds(format!("outline-{at}").leak())).collect();
        let (Some(first), Some(last)) = (bars.first(), bars.last()) else {
            panic!("{keyboard} pt of keyboard: no bar shows");
        };
        let (top, bottom) = (first.top(), last.bottom());
        assert!(
            top >= rail.top() && bottom <= rail.bottom(),
            "{keyboard} pt of keyboard: {top:?}..{bottom:?} in {rail:?}"
        );
    }
}

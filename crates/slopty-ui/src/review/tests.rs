//! The review tile in a headless window over a hub that holds a recorded review: the order of
//! its files, its two layouts, and what keeping, commenting and marking it reviewed send.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AppContext as _, Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, px, size,
};
use slopty_core::WallMs;
use slopty_proto::ClientMsg;
use slopty_proto::thread::detail::Hunk;
use slopty_proto::thread::wire::{
    FileDiff, Intent, IntentDone, Outcome, Review, ReviewScope, ThreadFrame, ThreadRequest,
};
use slopty_proto::thread::{Cap, Cursor, Delivery, Patch, ThreadId, TreeRef, TurnId};
use slopty_theme::Theme;

use super::view::{ReviewEvent, ReviewView};
use crate::conversation::ReviewWithAgent;
use crate::conversation::thread::{HubEvent, ThreadHub, fixtures};

type Sent = Rc<RefCell<Vec<ClientMsg>>>;

fn file(path: &str, lines: &[&str], added: u32, removed: u32) -> FileDiff {
    FileDiff {
        path: path.to_owned(),
        from: Some(format!("{path}@old")),
        to: Some(format!("{path}@new")),
        binary: false,
        patch: Patch {
            hunks: vec![Hunk {
                old_start: 10,
                old_lines: 3,
                new_start: 10,
                new_lines: 3,
                heading: None,
                lines: lines.iter().map(|l| (*l).to_owned()).collect(),
            }],
            added,
            removed,
            clipped_lines: 0,
            full: None,
        },
    }
}

/// The recorded review: a source file, a lock and a test.
fn review() -> Review {
    Review {
        scope: ReviewScope::Turn(TurnId(1)),
        from: None,
        to: None,
        files: vec![
            file("Cargo.lock", &[" a", "-b", "+c"], 1, 1),
            file("src/lib.rs", &[" fn main() {", "-    old();", "+    new();", " }"], 1, 1),
            file("tests/e2e.rs", &["+fn t() {}"], 1, 0),
        ],
        absent: None,
    }
}

/// A review tile `width` points wide over a linked hub whose thread has a turn and the
/// recorded review, and what the hub sent.
fn tile(
    cx: &mut TestAppContext,
    width: f32,
) -> (Entity<ReviewView>, Entity<ThreadHub>, Sent, &mut VisualTestContext) {
    let sent: Sent = Rc::default();
    let into = Rc::clone(&sent);
    let hub = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(crate::workspace::key_bindings());
        let hub = cx.new(|_| ThreadHub::new("studio".to_owned(), None));
        cx.subscribe(&hub, move |_hub, event: &HubEvent, _cx| {
            if let HubEvent::Send(msgs) = event {
                into.borrow_mut().extend(msgs.iter().cloned());
            }
        })
        .detach();
        hub
    });
    let state = fixtures::thread("edit");
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let held = hub.clone();
    let (view, cx) = cx.add_window_view(move |window, cx| {
        let mut view = ReviewView::new(held, thread, Theme::default(), window, cx);
        view.set_layout(1.0, width, cx);
        view
    });
    cx.simulate_resize(size(px(width), px(700.0)));
    let snapshot =
        ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 0 }, state: Box::new(state) };
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot, cx));
    hub.update(cx, |hub, cx| hub.frame(thread, ThreadFrame::Review(Box::new(review())), cx));
    cx.run_until_parked();
    (view, hub, sent, cx)
}

fn intents(sent: &Sent) -> Vec<Intent> {
    sent.borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent { intent, .. }) => Some(intent.clone()),
            _ => None,
        })
        .collect()
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} drawn")).center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

/// Opening the tile asks for the last turn's changes, and the files list the source first,
/// the lock and the test after it.
#[gpui::test]
fn the_tile_asks_for_the_last_turn_and_lists_the_source_first(cx: &mut TestAppContext) {
    let (_view, _hub, sent, cx) = tile(cx, 1200.0);
    let asked = sent.borrow().iter().any(|m| {
        matches!(m, ClientMsg::Thread(ThreadRequest::Review { scope: ReviewScope::Turn(_), .. }))
    });
    assert!(asked, "the last turn's review was asked for");
    let y = |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).unwrap().top();
    let (lib, lock, test) =
        (y(cx, "review-file-1"), y(cx, "review-file-0"), y(cx, "review-file-2"));
    assert!(lib < lock && lock < test, "the source, then the quiet ones by weight");
}

/// From 960 pt the lines pair side by side; under it they run in a column.
#[gpui::test]
fn the_sides_pair_from_960_points(cx: &mut TestAppContext) {
    let (_view, _hub, _sent, cx) = tile(cx, 1200.0);
    assert!(cx.debug_bounds("review-pair-1-0-0").is_some());
    assert!(cx.debug_bounds("review-line-1-0-0").is_none());
}

#[gpui::test]
fn under_960_points_the_lines_run_in_a_column(cx: &mut TestAppContext) {
    let (_view, _hub, _sent, cx) = tile(cx, 800.0);
    assert!(cx.debug_bounds("review-line-1-0-0").is_some());
    assert!(cx.debug_bounds("review-pair-1-0-0").is_none());
}

/// A change of appearance draws the same lines in the new theme.
#[gpui::test]
fn the_lines_stay_when_the_appearance_changes(cx: &mut TestAppContext) {
    let (view, _hub, _sent, cx) = tile(cx, 800.0);
    view.update(cx, |view, cx| view.set_theme(Theme::new(slopty_theme::Variant::Light), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-line-1-0-0").is_some(), "the lines in the light");
}

/// A keep goes as a pick of the hunk as the review showed it, and the hunk says so in that
/// frame.
#[gpui::test]
fn keeping_a_hunk_sends_its_pick_and_says_so_at_once(cx: &mut TestAppContext) {
    let (_view, _hub, sent, cx) = tile(cx, 1200.0);
    click(cx, "review-keep-hunk-1-0");
    assert_eq!(
        intents(&sent),
        [Intent::Keep(slopty_proto::thread::wire::Pick {
            path: "src/lib.rs".to_owned(),
            from: Some("src/lib.rs@old".to_owned()),
            stamp: Some("src/lib.rs@new".to_owned()),
            hunks: vec![0],
        })]
    );
    assert!(cx.debug_bounds("review-keep-hunk-1-0").is_none(), "Keeping, in its place");
}

/// A click on a line opens a comment there; Return keeps it; the foot sends every comment as
/// one message, each under the code it is on, and none wait after.
#[gpui::test]
fn line_comments_go_as_one_message(cx: &mut TestAppContext) {
    let (_view, _hub, sent, cx) = tile(cx, 800.0);
    click(cx, "review-line-1-0-2");
    assert!(cx.debug_bounds("review-draft").is_some(), "the field under the line");
    cx.simulate_input("Why new?");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-comment-0").is_some());
    click(cx, "review-send");
    assert_eq!(
        intents(&sent),
        [Intent::Send {
            text: "In `src/lib.rs` line 11:\n```diff\n+    new();\n```\nWhy new?".to_owned(),
            delivery: Delivery::Steer,
            attachments: vec![]
        }]
    );
    assert!(cx.debug_bounds("review-comment-0").is_none());
}

/// A drag over a hunk's lines comments on the run, quoted whole with its numbers; a
/// shift-press past it reaches further. "Add to message" hands the comments to the thread's
/// draft and sends nothing.
#[gpui::test]
fn a_drag_comments_on_a_run_and_add_to_message_sends_nothing(cx: &mut TestAppContext) {
    let (view, _hub, sent, cx) = tile(cx, 800.0);
    let heard: Rc<RefCell<Vec<ReviewEvent>>> = Rc::default();
    let into = Rc::clone(&heard);
    cx.update(|_w, cx| {
        cx.subscribe(&view, move |_view, event: &ReviewEvent, _cx| {
            into.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let at = |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).unwrap().center();
    let (from, to) = (at(cx, "review-line-1-0-0"), at(cx, "review-line-1-0-1"));
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    let past = at(cx, "review-line-1-0-2");
    cx.simulate_mouse_down(past, MouseButton::Left, Modifiers::shift());
    cx.simulate_mouse_up(past, MouseButton::Left, Modifiers::shift());
    cx.run_until_parked();
    cx.simulate_input("Why swap these?");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-comment-0").is_some(), "under the run's last line");

    click(cx, "review-add");
    assert!(intents(&sent).is_empty(), "nothing sent");
    assert_eq!(
        *heard.borrow(),
        [ReviewEvent::AddToMessage {
            thread: view.read_with(cx, |v, _| v.thread()),
            text: "In `src/lib.rs` lines 10\u{2013}11:\n```diff\n fn main() {\n-    old();\n+    \
                   new();\n```\nWhy swap these?"
                .to_owned(),
        }]
    );
    assert!(cx.debug_bounds("review-comment-0").is_none(), "none wait after");
}

/// "Mark reviewed" keeps every file shown.
#[gpui::test]
fn mark_reviewed_keeps_every_file(cx: &mut TestAppContext) {
    let (_view, _hub, sent, cx) = tile(cx, 1200.0);
    click(cx, "review-mark");
    let kept = intents(&sent)
        .iter()
        .filter(|i| matches!(i, Intent::Keep(p) if p.hunks.is_empty()))
        .count();
    assert_eq!(kept, 3);
}

/// A keep the worker turns down says why on the hunk it was for, in the frame the answer
/// comes, with Keep and Revert back beside it; trying again lets the old refusal go.
#[gpui::test]
fn a_refused_keep_says_why_on_its_hunk(cx: &mut TestAppContext) {
    let (_view, hub, sent, cx) = tile(cx, 1200.0);
    click(cx, "review-keep-hunk-1-0");
    let (id, thread) = hub
        .read_with(cx, |h, _| h.threads().outbox().all().first().map(|s| (s.id, s.thread)))
        .expect("the keep is on its way");
    let reason = "src/lib.rs changed since the review".to_owned();
    let done = IntentDone { id, outcome: Outcome::Refused { reason: reason.clone() } };
    hub.update(cx, |hub, cx| hub.done(&done, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-refused-hunk-1-0").is_some(), "said on its hunk");
    assert!(cx.debug_bounds("review-refused-file-1").is_none(), "not on the whole file");
    let words: Vec<String> =
        hub.read_with(cx, |h, _| h.refusals(thread).map(|r| r.words.clone()).collect());
    assert_eq!(words, [format!("Couldn't keep lib.rs: {reason}")]);
    assert!(cx.debug_bounds("review-keep-hunk-1-0").is_some(), "Keep is back to try again");

    click(cx, "review-keep-hunk-1-0");
    assert!(cx.debug_bounds("review-refused-hunk-1-0").is_none(), "the new try speaks now");
    assert_eq!(intents(&sent).len(), 2);
}

/// The tile asks the branch's pull request once, as it opens on a thread whose folder is
/// known, shows where it stands at the scope bar's end, and opens the commit sheet from there.
#[gpui::test]
fn the_tile_shows_the_branch_s_pull_request_and_opens_the_commit_sheet(cx: &mut TestAppContext) {
    use slopty_proto::git::{GitDone, GitOp, GitOutcome, PullStatus};
    let (_view, hub, sent, cx) = tile(cx, 1200.0);
    let reads: Vec<u64> = sent
        .borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Git { request, op: GitOp::PullStatus, .. } => Some(*request),
            _ => None,
        })
        .collect();
    assert_eq!(reads.len(), 1, "asked once, never polled");
    let pull = PullStatus {
        number: 12,
        url: "https://github.com/o/r/pull/12".to_owned(),
        title: "Refresh tokens".to_owned(),
        state: "OPEN".to_owned(),
        draft: false,
        head: "feature".to_owned(),
        head_commit: "abc".to_owned(),
        base: "main".to_owned(),
        review: "CHANGES_REQUESTED".to_owned(),
        mergeable: "MERGEABLE".to_owned(),
        merge_state: "BLOCKED".to_owned(),
        checks: Vec::new(),
        more_checks: 0,
    };
    let done = GitOutcome::Done(GitDone::PullStatus(Some(Box::new(pull))));
    hub.update(cx, |hub, cx| hub.git_done(reads[0], done, cx));
    cx.run_until_parked();
    click(cx, "review-pull");
    assert!(cx.debug_bounds("commit-sheet").is_some(), "the sheet opens over the review");
    assert!(cx.debug_bounds("commit-merge").is_none(), "no merge while changes are asked for");
}

/// A review tile like [`tile`], over a thread whose agent reviews through its own door, with
/// the recorded review's two trees, and the thread it is on.
fn door_tile(
    cx: &mut TestAppContext,
) -> (Entity<ReviewView>, Entity<ThreadHub>, Sent, &mut VisualTestContext) {
    let (view, hub, sent, cx) = tile(cx, 1200.0);
    let thread = view.read_with(cx, |v, _| v.thread());
    let mut state = fixtures::thread("edit");
    state.meta.id = thread;
    state.meta.caps.push(Cap::named(Cap::REVIEW));
    let snapshot =
        ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, state: Box::new(state) };
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot, cx));
    let mut trees = review();
    (trees.from, trees.to) = (Some(TreeRef("aa".to_owned())), Some(TreeRef("bb".to_owned())));
    hub.update(cx, |hub, cx| hub.frame(thread, ThreadFrame::Review(Box::new(trees)), cx));
    cx.run_until_parked();
    (view, hub, sent, cx)
}

/// The thread as it is once the agent has answered `answer` in a turn of its own and rests.
fn answered(hub: &Entity<ThreadHub>, cx: &mut VisualTestContext, thread: ThreadId, answer: &str) {
    use slopty_proto::thread::{
        Changed, Clipped, Item, ItemBody, ItemId, Phase, Turn, TurnState, Usage,
    };
    let mut state = hub
        .read_with(cx, |h, _| h.threads().mirror(thread).and_then(|m| m.state().cloned()))
        .expect("the thread");
    let turn = TurnId(state.turns.iter().map(|t| t.id.0).max().unwrap_or(0).saturating_add(1));
    state.turns.push(Turn {
        id: turn,
        input: None,
        state: TurnState::Complete,
        started_ms: WallMs::ZERO,
        ended_ms: Some(WallMs::ZERO),
        usage: Usage::default(),
        models: Vec::new(),
        changed: Changed::default(),
        before: None,
        after: None,
    });
    state.items.push(Item {
        id: ItemId(format!("answer-{}", turn.0)),
        turn,
        at_ms: WallMs::ZERO,
        body: ItemBody::Text(Clipped::whole(answer)),
    });
    state.status.phase = Phase::Idle;
    state.pending.clear();
    let snapshot =
        ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 9 }, state: Box::new(state) };
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot, cx));
    cx.run_until_parked();
}

fn reviews_asked(sent: &Sent) -> Vec<ReviewScope> {
    sent.borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Review { scope, .. }) => Some(*scope),
            _ => None,
        })
        .collect()
}

/// "Review with `<agent>`" shows only where the thread's agent has its own review door, and is
/// a line of the palette there and nowhere else.
#[gpui::test]
fn review_with_the_agent_shows_only_where_it_has_a_door(cx: &mut TestAppContext) {
    let (view, _hub, _sent, cx) = tile(cx, 1200.0);
    assert!(cx.debug_bounds("review-by-agent").is_none(), "no door, no button");
    let offered = |cx: &mut VisualTestContext| {
        let focus = view.read_with(cx, gpui::Focusable::focus_handle);
        cx.update(|window, cx| {
            window.focus(&focus, cx);
            window.is_action_available(&ReviewWithAgent, cx)
        })
    };
    assert!(!offered(cx), "nor a palette line");

    let (view, _hub, _sent, cx) = door_tile(cx);
    assert!(cx.debug_bounds("review-by-agent").is_some());
    assert_eq!(view.read_with(cx, ReviewView::door).as_deref(), Some("Claude Code"));
    let focus = view.read_with(cx, gpui::Focusable::focus_handle);
    let available = cx.update(|window, cx| {
        window.focus(&focus, cx);
        window.is_action_available(&ReviewWithAgent, cx)
    });
    assert!(available, "the palette's line");
}

/// The press asks the agent for its own review of the two trees on show and says it runs;
/// the span stays on show though the thread moves on. Once the agent rests, each finding on a
/// line in the diff is a comment under it, marked as the agent's, one about a file not on show
/// is a note above the diff, and the band says what came. Every one goes back as one message.
#[gpui::test]
fn the_agents_findings_become_comments_and_notes_sent_as_one(cx: &mut TestAppContext) {
    let (view, hub, sent, cx) = door_tile(cx);
    let thread = view.read_with(cx, |v, _| v.thread());
    click(cx, "review-by-agent");
    assert_eq!(
        intents(&sent),
        [Intent::Review { from: TreeRef("aa".to_owned()), to: TreeRef("bb".to_owned()) }]
    );
    assert!(cx.debug_bounds("review-by-agent-running").is_some(), "it says it runs");
    assert!(cx.debug_bounds("review-by-agent").is_none());
    let asked = reviews_asked(&sent).len();

    answered(
        &hub,
        cx,
        thread,
        "Two things.\n\n\
         - [P1] Call the new one only once \u{2014} /w/src/lib.rs:11-11\n  It runs twice on retry.\n\
         - [P3] Say it in the README \u{2014} /w/README.md:3-3\n  The old name is still there.",
    );
    assert_eq!(reviews_asked(&sent).len(), asked, "the span on show stays, not the new turn's");
    assert!(cx.debug_bounds("review-by-agent-running").is_none());
    assert!(cx.debug_bounds("review-comment-0").is_some(), "on its line");
    assert!(cx.debug_bounds("review-note-0").is_some(), "not on show: a note");
    let came = cx.debug_bounds("review-came").is_some();
    assert!(came, "the band says what came");
    let heard = said(cx);
    assert!(heard.iter().any(|l| l == "Claude Code raised 2 findings"), "{heard:?}");

    click(cx, "review-send");
    let sent_words = intents(&sent).into_iter().rev().find_map(|i| match i {
        Intent::Send { text, .. } => Some(text),
        _ => None,
    });
    assert_eq!(
        sent_words.as_deref(),
        Some(
            "In `src/lib.rs` line 11:\n```diff\n+    new();\n```\n[P1] Call the new one only once\n\
             It runs twice on retry.\n\nIn `/w/README.md:3`:\n[P3] Say it in the README\nThe old \
             name is still there."
        )
    );
    assert!(cx.debug_bounds("review-findings").is_none(), "nothing waits after");
}

/// A finding the person lets go is gone from what is sent; a review that raised nothing says
/// what the agent said of it; a refused one says why, in the error's tone.
#[gpui::test]
fn findings_are_let_go_and_a_review_says_how_it_came_out(cx: &mut TestAppContext) {
    let (view, hub, _sent, cx) = door_tile(cx);
    let thread = view.read_with(cx, |v, _| v.thread());
    click(cx, "review-by-agent");
    answered(&hub, cx, thread, "- Nothing placed here\n- Another loose one");
    assert!(cx.debug_bounds("review-note-1").is_some(), "kept, though they name no place");
    click(cx, "review-unnote-0");
    click(cx, "review-unnote-0");
    assert!(cx.debug_bounds("review-note-0").is_none());
    click(cx, "review-came-close");
    assert!(cx.debug_bounds("review-findings").is_none(), "all let go");

    click(cx, "review-by-agent");
    answered(&hub, cx, thread, "No issues found. The change does what it says.");
    let heard = said(cx);
    assert!(
        heard
            .iter()
            .any(|l| l
                == "Claude Code raised nothing: No issues found. The change does what it says."),
        "{heard:?}"
    );

    click(cx, "review-came-close");
    click(cx, "review-by-agent");
    let id = hub
        .read_with(cx, |h, _| h.threads().outbox().all().last().map(|s| s.id))
        .expect("the review is on its way");
    let done = IntentDone {
        id,
        outcome: Outcome::Refused { reason: "Codex is still working on this thread".to_owned() },
    };
    hub.update(cx, |hub, cx| hub.done(&done, cx));
    cx.run_until_parked();
    let heard = said(cx);
    assert!(
        heard.iter().any(|l| l == "The review didn't start: Codex is still working on this thread"),
        "{heard:?}"
    );
    assert!(cx.debug_bounds("review-by-agent").is_some(), "it can be asked again");
}

/// What the tile says to a screen reader, every label in its tree.
fn said(cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|window, _cx| {
        window.set_a11y_active(true);
        window.refresh();
    });
    cx.run_until_parked();
    cx.update(|window, _cx| crate::a11y::tree(window)).into_iter().filter_map(|n| n.label).collect()
}

/// The review asks who wrote its files' lines as the diff ends, once each; the answer names a
/// line's turn at its end while the pointer is on it, and a press on that opens the thread at
/// the turn, commenting on nothing. A line no thread wrote names none.
#[gpui::test]
fn a_line_names_the_turn_that_wrote_it_under_the_pointer(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::{AuthorRun, Authors};

    use crate::authorship::Opens;

    let (view, hub, sent, cx) = tile(cx, 800.0);
    let thread = view.read_with(cx, |v, _| v.thread());
    let asked: Vec<String> = sent
        .borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Authors { thread: Some(t), path }) if *t == thread => {
                Some(path.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(asked.len(), 3, "each file once: {asked:?}");
    assert!(asked.contains(&"src/lib.rs".to_owned()));

    let heard: Rc<RefCell<Vec<ReviewEvent>>> = Rc::default();
    let into = Rc::clone(&heard);
    cx.update(|_w, cx| {
        cx.subscribe(&view, move |_view, event: &ReviewEvent, _cx| {
            into.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let authors = Authors {
        thread: Some(thread),
        path: "src/lib.rs".to_owned(),
        modified_ms: None,
        blob: Some("src/lib.rs@new".to_owned()),
        runs: vec![AuthorRun {
            start: 11,
            lines: 1,
            thread,
            turn: Some(TurnId(1)),
            commit: None,
            at_ms: WallMs::now(),
        }],
        absent: None,
    };
    hub.update(cx, |hub, cx| hub.heard_authors(authors, cx));
    cx.run_until_parked();
    let center = |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).unwrap().center();
    // The first line, `fn main() {`, is no thread's.
    let first = center(cx, "review-line-1-0-0");
    cx.simulate_mouse_move(first, None, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-line-1-0-0-author").is_none());
    let written = center(cx, "review-line-1-0-2");
    cx.simulate_mouse_move(written, None, Modifiers::none());
    cx.run_until_parked();
    click(cx, "review-line-1-0-2-author");
    assert_eq!(*heard.borrow(), [ReviewEvent::OpenThread(Opens { thread, turn: Some(TurnId(1)) })]);
    assert!(cx.debug_bounds("review-draft").is_none(), "no comment started");
    assert!(intents(&sent).is_empty(), "nothing sent");
}

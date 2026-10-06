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

/// The tile's height in every test, in points.
const HEIGHT: f32 = 700.0;

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

/// The recorded review: a source file of two hunks, a lock and a test.
fn review() -> Review {
    let mut lib = file("src/lib.rs", &[" fn main() {", "-    old();", "+    new();", " }"], 1, 1);
    lib.patch.hunks.push(Hunk {
        old_start: 40,
        old_lines: 1,
        new_start: 40,
        new_lines: 2,
        heading: Some("fn tail() {".to_owned()),
        lines: vec!["     done();".to_owned(), "+    log();".to_owned()],
    });
    lib.patch.added = 2;
    Review {
        scope: ReviewScope::Turn(TurnId(1)),
        from: None,
        to: None,
        files: vec![
            file("Cargo.lock", &[" a", "-b", "+c"], 1, 1),
            lib,
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
    tile_in(cx, width, None)
}

/// [`tile`], its thread working in `cwd` where given.
fn tile_in<'a>(
    cx: &'a mut TestAppContext,
    width: f32,
    cwd: Option<&str>,
) -> (Entity<ReviewView>, Entity<ThreadHub>, Sent, &'a mut VisualTestContext) {
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
    let mut state = fixtures::thread("edit");
    if let Some(cwd) = cwd {
        cwd.clone_into(&mut state.meta.cwd);
    }
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let held = hub.clone();
    let (view, cx) = cx.add_window_view(move |window, cx| {
        let mut view = ReviewView::new(held, thread, Theme::default(), window, cx);
        view.set_layout(1.0, width, HEIGHT, cx);
        view
    });
    cx.simulate_resize(size(px(width), px(HEIGHT)));
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

/// The worker's answer to the last message the tile sent.
fn answer_send(hub: &Entity<ThreadHub>, sent: &Sent, outcome: Outcome, cx: &mut VisualTestContext) {
    let id = sent
        .borrow()
        .iter()
        .rev()
        .find_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent {
                id, intent: Intent::Send { .. }, ..
            }) => Some(*id),
            _ => None,
        })
        .expect("a message sent");
    hub.update(cx, |hub, cx| hub.done(&IntentDone { id, outcome }, cx));
    cx.run_until_parked();
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

/// A review is one plane, as the status-colour study ruled (`docs/decisions/ui.md`, "Space,
/// headings and tone part the review and the thread"): no hairline frames a file, rules the
/// scope bar or the foot, or parts the file list from the diff. A border that runs most of the
/// way across the tile, or down the file list's edge, fails here; a button's own edge is too
/// short to.
#[gpui::test]
fn no_rule_or_frame_parts_the_review(cx: &mut TestAppContext) {
    for width in [1200.0, 800.0] {
        let (_view, _hub, _sent, cx) = tile(cx, width);
        let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
        let ruled: Vec<String> = quads
            .iter()
            .filter(|q| {
                let w = q.border_widths;
                let (across, down) =
                    (q.bounds.size.width.0 / scale, q.bounds.size.height.0 / scale);
                let horizontal = (w.top.0 > 0.0 || w.bottom.0 > 0.0) && across > width * 0.4;
                let vertical = (w.left.0 > 0.0 || w.right.0 > 0.0) && down > HEIGHT * 0.4;
                horizontal || vertical
            })
            .map(|q| format!("{:?} {:?}", q.bounds, q.border_widths))
            .collect();
        assert!(ruled.is_empty(), "at {width} pt: {}", ruled.join("\n"));
        assert!(cx.debug_bounds("review-head-row-1").is_some(), "a file's head is drawn");
    }
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

/// A file of one hunk keeps and puts back from its head alone. In a file of more, a hunk's
/// own keep and put back show while the pointer is on its lines, and only that hunk's.
#[gpui::test]
fn a_hunks_keep_shows_with_the_pointer_and_never_twice(cx: &mut TestAppContext) {
    let (view, _hub, _sent, cx) = tile(cx, 800.0);
    assert!(cx.debug_bounds("review-keep-file-0").is_some(), "the lock's head keeps it");
    assert!(cx.debug_bounds("review-keep-hunk-0-0").is_none(), "and nothing else does");
    assert!(cx.debug_bounds("review-keep-hunk-1-0").is_some(), "in its place while hidden");
    let shown = |cx: &mut VisualTestContext, hunk| view.read_with(cx, |v, _| v.hunk_shown(1, hunk));
    assert!(!shown(cx, 0) && !shown(cx, 1), "out of the way of the code");

    let on = cx.debug_bounds("review-line-1-0-1").expect("the hunk's line").center();
    cx.simulate_mouse_move(on, None, Modifiers::none());
    cx.run_until_parked();
    assert!(shown(cx, 0), "the pointer on its line shows them");
    assert!(!shown(cx, 1), "only that hunk's");
}

/// A click on a line opens a comment there; Return keeps it; the foot sends every comment as
/// one message, each under the code it is on, and none wait once the worker took it.
#[gpui::test]
fn line_comments_go_as_one_message(cx: &mut TestAppContext) {
    let (_view, hub, sent, cx) = tile(cx, 800.0);
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
    assert!(cx.debug_bounds("review-comment-0").is_some(), "kept until the worker takes it");
    answer_send(&hub, &sent, Outcome::Accepted, cx);
    assert!(cx.debug_bounds("review-comment-0").is_none());
}

/// A comment keeps its lines: a pasted suggestion keeps its line ends and ⇧↵ breaks a line,
/// where ↵ still adds the comment. They go to the agent as written.
#[gpui::test]
fn a_comment_keeps_its_lines(cx: &mut TestAppContext) {
    let (_view, _hub, sent, cx) = tile(cx, 800.0);
    click(cx, "review-line-1-0-2");
    cx.simulate_input("Rather:\n    renewed();");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("Then test it.");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-comment-0").is_some(), "↵ added it");
    click(cx, "review-send");
    assert_eq!(
        intents(&sent),
        [Intent::Send {
            text: "In `src/lib.rs` line 11:\n```diff\n+    new();\n```\n\
                   Rather:\n    renewed();\nThen test it."
                .to_owned(),
            delivery: Delivery::Steer,
            attachments: vec![]
        }]
    );
}

/// The comments go as the thread's composer sends a message now: an agent that takes no
/// message mid-turn but queues (ACP's) is sent them queued, never as a steer it would refuse.
#[gpui::test]
fn comments_to_an_agent_with_no_steer_go_queued(cx: &mut TestAppContext) {
    let (_view, hub, sent, cx) = tile(cx, 800.0);
    let mut state = fixtures::thread("edit");
    state.meta.caps = [Cap::APPROVALS, Cap::INTERRUPT, Cap::QUEUE, Cap::SNAPSHOTS]
        .iter()
        .map(|c| Cap::named(c))
        .collect();
    let thread = state.meta.id;
    let snapshot =
        ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, state: Box::new(state) };
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot, cx));
    cx.run_until_parked();
    click(cx, "review-line-1-0-2");
    cx.simulate_input("Why new?");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    click(cx, "review-send");
    let sent = intents(&sent);
    let [Intent::Send { delivery, .. }] = sent.as_slice() else { panic!("one message: {sent:?}") };
    assert_eq!(*delivery, Delivery::Queue);
}

/// With comments waiting, the foot shows its three buttons where they fit, at their shaped
/// widths. Narrower, "Mark reviewed" goes behind "More" first, then "Add to message"; the send
/// stays whole inside the tile, and "More" offers exactly what left.
#[gpui::test]
fn a_narrow_foot_keeps_the_send_and_folds_the_rest(cx: &mut TestAppContext) {
    for (width, shown) in [
        (800.0, &["review-add", "review-mark"][..]),
        (360.0, &["review-add"][..]),
        (240.0, &[][..]),
    ] {
        let (_view, _hub, _sent, cx) = tile(cx, width);
        click(cx, "review-line-1-0-2");
        cx.simulate_input("Why new?");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let drawn = |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s);
        let send = drawn(cx, "review-send").expect("the send always shows");
        assert!(f32::from(send.right()) <= width, "{width}: the send inside the tile {send:?}");
        let left: Vec<&str> =
            ["review-add", "review-mark"].into_iter().filter(|p| !shown.contains(p)).collect();
        for part in ["review-add", "review-mark"] {
            assert_eq!(drawn(cx, part).is_some(), shown.contains(&part), "{width}: {part}");
        }
        assert_eq!(drawn(cx, "review-more").is_some(), !left.is_empty(), "{width}: More");
        for part in ["review-add", "review-mark", "review-more"] {
            if let Some(b) = drawn(cx, part) {
                assert!(f32::from(b.right()) <= width, "{width}: {part} inside the tile {b:?}");
            }
        }
        if !left.is_empty() {
            click(cx, "review-more");
            for part in left {
                let row = if part == "review-add" { "review-more-add" } else { "review-more-mark" };
                assert!(drawn(cx, row).is_some(), "{width}: {part} waits behind More");
            }
        }
    }
}

/// In a column beside a board the scope bar keeps the span on show and its refresh, each word
/// whole on one line, and the rest leave for "More", which offers them; with room every span
/// shows.
#[gpui::test]
fn a_narrow_scope_bar_keeps_the_span_on_show(cx: &mut TestAppContext) {
    const SCOPES: [(&str, &str); 3] = [
        ("review-scope-LastTurn", "review-scopes-more-LastTurn"),
        ("review-scope-SinceReviewed", "review-scopes-more-SinceReviewed"),
        ("review-scope-AllTurns", "review-scopes-more-AllTurns"),
    ];
    for (width, all) in [(800.0, true), (200.0, false)] {
        let (_view, _hub, _sent, cx) = tile(cx, width);
        let scopes: Vec<_> = SCOPES.iter().map(|(s, row)| (*s, *row, cx.debug_bounds(s))).collect();
        let shown = scopes.iter().filter(|(.., b)| b.is_some()).count();
        assert!(shown >= 1, "{width}: the span on show stays");
        assert_eq!(shown == scopes.len(), all, "{width}: {scopes:?}");
        for (scope, _, b) in &scopes {
            if let Some(b) = b {
                assert!(f32::from(b.right()) <= width, "{width}: {scope} inside {b:?}");
                assert!(f32::from(b.size.height) < 24.0, "{width}: {scope} on one line {b:?}");
            }
        }
        assert_eq!(cx.debug_bounds("review-scopes-more").is_some(), !all, "{width}: More");
        if !all {
            click(cx, "review-scopes-more");
            for (scope, row, _) in scopes.iter().filter(|(.., b)| b.is_none()) {
                assert!(cx.debug_bounds(row).is_some(), "{width}: {scope} waits behind More");
            }
        }
    }
}

/// A send of the comments the worker turns down loses none of them: they stay on their lines,
/// the band says why, and the foot sends them again.
#[gpui::test]
fn comments_whose_send_is_turned_down_stay(cx: &mut TestAppContext) {
    let (_view, hub, sent, cx) = tile(cx, 800.0);
    click(cx, "review-line-1-0-2");
    cx.simulate_input("Why new?");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    click(cx, "review-send");
    let refused = Outcome::Refused { reason: "the agent has exited".to_owned() };
    answer_send(&hub, &sent, refused, cx);
    assert!(cx.debug_bounds("review-comment-0").is_some(), "the comment stays");
    let heard = said(cx);
    let why = format!("{}: the agent has exited", super::view::NOT_SENT);
    assert!(heard.contains(&why), "{heard:?}");
    click(cx, "review-send");
    let sends = intents(&sent).iter().filter(|i| matches!(i, Intent::Send { .. })).count();
    assert_eq!(sends, 2, "sent again");
}

/// A drag over a hunk's lines comments on the run, quoted whole with its numbers; a
/// shift-press past it reaches further. "Add to message" hands the comments to the thread's
/// draft and sends nothing; they go once a composer took them, and stay when none did.
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
            thread: view.read_with(cx, |v, _| v.thread()).expect("a thread's review"),
            text: "In `src/lib.rs` lines 10\u{2013}11:\n```diff\n fn main() {\n-    old();\n+    \
                   new();\n```\nWhy swap these?"
                .to_owned(),
            id: 1,
        }]
    );
    assert!(cx.debug_bounds("review-comment-0").is_some(), "kept until a composer takes them");
    view.update(cx, |v, cx| v.added(1, false, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-comment-0").is_some(), "no composer took them: they stay");
    click(cx, "review-add");
    view.update(cx, |v, cx| v.added(2, true, cx));
    cx.run_until_parked();
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
    let thread = view.read_with(cx, |v, _| v.thread()).expect("a thread's review");
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
            ClientMsg::Thread(ThreadRequest::Review { scope, .. }) => Some(scope.clone()),
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
    let thread = view.read_with(cx, |v, _| v.thread()).expect("a thread's review");
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
    answer_send(&hub, &sent, Outcome::Accepted, cx);
    assert!(cx.debug_bounds("review-findings").is_none(), "nothing waits after");
}

/// Findings that would take more than a quarter of the diff's room fold to one line that says
/// what came; it opens them and folds them again, and the code keeps the tile meanwhile.
#[gpui::test]
fn many_findings_fold_to_one_line_that_opens_them(cx: &mut TestAppContext) {
    let (view, hub, _sent, cx) = door_tile(cx);
    let thread = view.read_with(cx, |v, _| v.thread()).expect("a thread's review");
    click(cx, "review-by-agent");
    let found: Vec<String> = (1..=6)
        .map(|n| {
            format!(
                "- [P2] Say it in the guide, part {n} \u{2014} /w/docs/guide-{n}.md:3-3\n  The old \
                 name is still there, and the example under it calls the call that went away."
            )
        })
        .collect();
    answered(&hub, cx, thread, &found.join("\n"));
    assert!(cx.debug_bounds("review-findings-toggle").is_some(), "folded to its line");
    assert!(cx.debug_bounds("review-note-0").is_none(), "the findings wait under it");
    let heard = said(cx);
    assert!(heard.iter().any(|l| l == "Claude Code raised 6 findings"), "{heard:?}");

    click(cx, "review-findings-toggle");
    assert!(cx.debug_bounds("review-note-5").is_some(), "open on demand");
    click(cx, "review-findings-toggle");
    assert!(cx.debug_bounds("review-note-0").is_none(), "and folded again");
}

/// A finding the person lets go is gone from what is sent; a review that raised nothing says
/// what the agent said of it; a refused one says why, in the error's tone.
#[gpui::test]
fn findings_are_let_go_and_a_review_says_how_it_came_out(cx: &mut TestAppContext) {
    let (view, hub, _sent, cx) = door_tile(cx);
    let thread = view.read_with(cx, |v, _| v.thread()).expect("a thread's review");
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
    let thread = view.read_with(cx, |v, _| v.thread()).expect("a thread's review");
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
    assert!(cx.debug_bounds("review-author-1-0-0").is_none());
    let written = center(cx, "review-line-1-0-2");
    cx.simulate_mouse_move(written, None, Modifiers::none());
    cx.run_until_parked();
    click(cx, "review-author-1-0-2");
    assert_eq!(*heard.borrow(), [ReviewEvent::OpenThread(Opens { thread, turn: Some(TurnId(1)) })]);
    assert!(cx.debug_bounds("review-draft").is_none(), "no comment started");
    assert!(intents(&sent).is_empty(), "nothing sent");
}

/// A span that changed nothing says so by name, as a finding and not as an empty pane, and
/// every span's words are sentence case.
#[gpui::test]
fn an_empty_review_names_its_span(cx: &mut TestAppContext) {
    let (view, hub, _sent, cx) = tile(cx, 1200.0);
    let thread = view.read_with(cx, |view, _cx| view.thread()).expect("a thread's review");
    let nothing = Review { files: Vec::new(), ..review() };
    hub.update(cx, |hub, cx| hub.frame(thread, ThreadFrame::Review(Box::new(nothing)), cx));
    cx.run_until_parked();
    let words = said(cx);
    assert!(words.iter().any(|w| w == "The last turn changed no files"), "{words:?}");
    for scope in super::model::Scope::ALL {
        let text = scope.nothing();
        let rest: String = text.chars().skip(1).collect();
        assert!(!rest.chars().any(char::is_uppercase), "sentence case: {text:?}");
    }
}

/// An empty span says so as one block in the tile's middle, and offers the widest span of its
/// kind as the one next step, which switches to it; the widest span offers none. In a column
/// beside a board the block keeps inside the tile.
#[gpui::test]
fn an_empty_span_offers_the_widest(cx: &mut TestAppContext) {
    let (view, hub, _sent, cx) = tile(cx, 312.0);
    let thread = view.read_with(cx, |view, _cx| view.thread()).expect("a thread's review");
    let nothing = Review { files: Vec::new(), ..review() };
    hub.update(cx, |hub, cx| hub.frame(thread, ThreadFrame::Review(Box::new(nothing)), cx));
    cx.run_until_parked();
    let widen = cx.debug_bounds("review-widen").expect("the way to every turn");
    let empty = cx.debug_bounds("review-empty").expect("the empty state");
    assert!(empty.contains(&widen.center()) && f32::from(widen.right()) <= 312.0, "{widen:?}");
    click(cx, "review-widen");
    assert_eq!(view.read_with(cx, |v, _| v.scope()), super::model::Scope::AllTurns);
}

/// A file's own menu, by a right click on its row in the list or on its head: Keep, Open and
/// Copy path, then Revert set apart, saying the file and the point it goes back to, since
/// nothing undoes it. The review's paths start at the repository's root, which the tile reads
/// as it opens: until that is known nothing opens, and the copy says it copies the path in the
/// repository.
#[gpui::test]
fn a_files_menu_keeps_opens_copies_and_says_what_revert_does(cx: &mut TestAppContext) {
    use slopty_proto::git::{GitDone, GitOp, GitOutcome, GitStatus};

    use super::view::{COPY_PATH, COPY_PATH_IN_REPOSITORY, revert_words};
    use crate::review::model::Scope;

    let (view, hub, sent, cx) = tile_in(cx, 1200.0, Some("/r/crates"));
    let heard: Rc<RefCell<Vec<ReviewEvent>>> = Rc::default();
    let into = Rc::clone(&heard);
    cx.update(|_w, cx| {
        cx.subscribe(&view, move |_view, event: &ReviewEvent, _cx| {
            into.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let right_click = |cx: &mut VisualTestContext, selector: &'static str| {
        let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} drawn")).center();
        cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
        cx.run_until_parked();
    };
    let rows = |cx: &mut VisualTestContext| -> Vec<String> {
        cx.update(|window, _cx| {
            window.set_a11y_active(true);
            window.refresh();
        });
        cx.run_until_parked();
        cx.update(|window, _cx| crate::a11y::tree(window))
            .into_iter()
            .filter(|n| n.role == "MenuItem")
            .filter_map(|n| n.label)
            .collect()
    };
    let copied = |cx: &mut VisualTestContext| {
        cx.update(|_w, cx| cx.read_from_clipboard().and_then(|i| i.text()))
    };
    let revert = revert_words("lib.rs", Scope::LastTurn);
    assert_eq!(revert, "Revert lib.rs to before the last turn");

    // `src/lib.rs` is the review's second file.
    right_click(cx, "review-file-1");
    assert!(cx.debug_bounds("review-file-menu").is_some(), "the menu hangs at the press");
    assert_eq!(rows(cx), ["Keep", COPY_PATH_IN_REPOSITORY, revert.as_str()], "no root yet");
    click(cx, "review-file-menu-copy-path");
    assert!(cx.debug_bounds("review-file-menu").is_none(), "a pick closes it");
    assert_eq!(copied(cx).as_deref(), Some("src/lib.rs"));

    let status: Vec<u64> = sent
        .borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Git { request, op: GitOp::Status, .. } => Some(*request),
            _ => None,
        })
        .collect();
    assert_eq!(status.len(), 1, "the root is asked once, as the tile opens");
    let root = GitStatus {
        root: "/r".to_owned(),
        branch: Some("main".to_owned()),
        head: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        files: Vec::new(),
        more: 0,
    };
    let done = GitOutcome::Done(GitDone::Status(Box::new(root)));
    hub.update(cx, |hub, cx| hub.git_done(status[0], done, cx));
    cx.run_until_parked();

    right_click(cx, "review-head-row-1");
    assert_eq!(rows(cx), ["Keep", "Open", COPY_PATH, revert.as_str()]);
    click(cx, "review-file-menu-open");
    let opened = ReviewEvent::OpenFile { path: "/r/src/lib.rs".to_owned() };
    assert_eq!(*heard.borrow(), [opened], "from the root, not the thread's folder");
    right_click(cx, "review-file-1");
    click(cx, "review-file-menu-copy-path");
    assert_eq!(copied(cx).as_deref(), Some("/r/src/lib.rs"));

    right_click(cx, "review-file-1");
    click(cx, "review-file-menu-revert");
    let reverted: Vec<Intent> =
        intents(&sent).into_iter().filter(|i| matches!(i, Intent::Revert(_))).collect();
    assert!(
        matches!(&*reverted, [Intent::Revert(p)] if p.path == "src/lib.rs" && p.hunks.is_empty()),
        "the whole file goes back: {reverted:?}"
    );
    right_click(cx, "review-file-1");
    assert_eq!(rows(cx), ["Open", COPY_PATH], "no keep or revert while one is on its way");
}

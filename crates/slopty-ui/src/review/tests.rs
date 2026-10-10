//! The review tile in a headless window over a hub that holds a recorded review: the order of
//! its files, its folds, and what keeping, commenting and marking it reviewed send.

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
        kind: slopty_proto::thread::wire::FileKind::Text,
        old_path: None,
        modes: None,
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
        view.set_layout(width, HEIGHT, cx);
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

/// The diff is unified at every width: no side-by-side rows, wide or narrow.
#[gpui::test]
fn the_diff_is_unified_at_every_width(cx: &mut TestAppContext) {
    for width in [1600.0, 800.0] {
        let (_view, _hub, _sent, cx) = tile(cx, width);
        assert!(cx.debug_bounds("review-line-1-0-0").is_some(), "{width}: one column");
        assert!(cx.debug_bounds("review-pair-1-0-0").is_none(), "{width}: no pairs");
    }
}

/// A file's head folds it to the head and opens it again; the scope bar's switch folds every
/// file, then, with all folded, opens them all; picking a folded file in the list opens it.
#[gpui::test]
fn files_fold_to_their_heads_one_or_all(cx: &mut TestAppContext) {
    let (view, _hub, _sent, cx) = tile(cx, 1200.0);
    let shown = |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).is_some();
    click(cx, "review-fold-1");
    assert!(!shown(cx, "review-line-1-0-0"), "the file folded to its head");
    assert!(shown(cx, "review-head-1") && shown(cx, "review-line-0-0-0"), "the rest stay");
    click(cx, "review-fold-1");
    assert!(shown(cx, "review-line-1-0-0"), "and opened again");

    click(cx, "review-fold-all");
    for line in ["review-line-0-0-0", "review-line-1-0-0", "review-line-2-0-0"] {
        assert!(!shown(cx, line), "{line}: every file folded");
    }
    assert!(view.read_with(cx, |v, _| v.all_folded()));
    click(cx, "review-file-2");
    assert!(shown(cx, "review-line-2-0-0"), "the file picked in the list opened");
    assert!(!view.read_with(cx, |v, _| v.all_folded()));

    click(cx, "review-fold-all");
    assert!(view.read_with(cx, |v, _| v.all_folded()), "one open: the switch folds all");
    click(cx, "review-fold-all");
    for line in ["review-line-0-0-0", "review-line-1-0-0", "review-line-2-0-0"] {
        assert!(shown(cx, line), "{line}: every file open");
    }
}

/// The unchanged lines between hunks fold to one line each, before the first hunk and between
/// two. Opening one asks the worker for the file's new side by its blob, through the thread,
/// and draws the stretch's lines as they come. (Where the stretches lie, past the last hunk
/// too, is `gaps::tests::the_stretches_lie_between_the_hunks`.)
#[gpui::test]
fn the_lines_between_hunks_open_from_the_file_s_blob(cx: &mut TestAppContext) {
    use slopty_proto::thread::ContentRef;
    use slopty_proto::thread::wire::Expanded;

    let (_view, hub, sent, cx) = tile(cx, 1200.0);
    let thread = sent
        .borrow()
        .iter()
        .find_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Review { thread, .. }) => Some(*thread),
            _ => None,
        })
        .expect("the review was asked");
    for gap in ["review-gap-1-0", "review-gap-1-1"] {
        assert!(cx.debug_bounds(gap).is_some(), "{gap}: a fold");
    }
    assert!(cx.debug_bounds("review-gap-1-2").is_none(), "the file's length is not known");
    click(cx, "review-gap-1-1");
    let content = ContentRef::blob("src/lib.rs@new");
    let asked = sent.borrow().iter().any(|m| {
        matches!(m, ClientMsg::Thread(ThreadRequest::Expand { content: c, .. }) if *c == content)
    });
    assert!(asked, "the new side, asked by its blob");

    let text = (1..=50).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
    let body = Expanded::Text(text);
    hub.update(cx, |hub, cx| hub.frame(thread, ThreadFrame::Expanded { content, body }, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-gap-1-1").is_none(), "the fold opened");
    assert!(cx.debug_bounds("review-context-1-1-0").is_some(), "its first line");
    assert!(cx.debug_bounds("review-gap-1-0").is_some(), "the other stays folded");
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
            old_path: None,
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
            if !matches!(event, ReviewEvent::Drafted) {
                into.borrow_mut().push(event.clone());
            }
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
        forge: slopty_proto::git::Forge::GitHub,
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
            if !matches!(event, ReviewEvent::Drafted) {
                into.borrow_mut().push(event.clone());
            }
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
            if !matches!(event, ReviewEvent::Drafted) {
                into.borrow_mut().push(event.clone());
            }
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
        forge: None,
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

/// A file the review's line budget left without hunks says how many lines it has, not "No
/// lines to show", and a press reads it whole from the repository by its blobs: "Reading…" on
/// its way, then its hunks in place of the row. One that could not be read says why and can be
/// asked again.
#[gpui::test]
fn a_file_past_the_review_s_budget_says_so_and_comes_whole_on_a_press(cx: &mut TestAppContext) {
    use slopty_proto::git::{GitDone, GitOp, GitOutcome};

    let (_view, hub, sent, cx) = tile_in(cx, 1200.0, Some("/w/repo"));
    let thread = sent
        .borrow()
        .iter()
        .find_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Review { thread, .. }) => Some(*thread),
            _ => None,
        })
        .expect("the review was asked");
    let left_out = |path: &str, lines: u32| {
        let mut file = file(path, &[], lines, 0);
        file.patch.hunks.clear();
        file.patch.clipped_lines = lines;
        file
    };
    let mut big = review();
    // Read first by weight: at 0 and 1 of the review, listed before the rest.
    big.files.insert(0, left_out("src/big.rs", 25_000));
    big.files.insert(1, left_out("src/huge.rs", 30_000));
    hub.update(cx, |hub, cx| hub.frame(thread, ThreadFrame::Review(Box::new(big)), cx));
    cx.run_until_parked();
    let row = cx.debug_bounds("review-left-out-0").expect("the file left out has its row");
    assert!(cx.debug_bounds("review-whole-0").is_some(), "with the press that reads it");
    assert!(row.size.height > px(0.0));

    sent.borrow_mut().clear();
    click(cx, "review-whole-0");
    let asked = sent.borrow().iter().find_map(|m| match m {
        ClientMsg::Git { request, repo, op: GitOp::FileDiff { from, to } } => {
            Some((*request, repo.clone(), from.clone(), to.clone()))
        }
        _ => None,
    });
    let Some((request, repo, from, to)) = asked else { panic!("asked whole: {sent:?}") };
    assert_eq!(repo, "/w/repo", "of the thread's repository");
    assert_eq!(
        (from.as_deref(), to.as_deref()),
        (Some("src/big.rs@old"), Some("src/big.rs@new")),
        "by the blobs the review named"
    );
    assert!(cx.debug_bounds("review-whole-said-0").is_some(), "reading, on its way");
    assert!(cx.debug_bounds("review-whole-0").is_none(), "asked once");

    let patch = Patch {
        hunks: vec![Hunk {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 2,
            heading: None,
            lines: vec!["+big one".to_owned(), "+big two".to_owned()],
        }],
        added: 2,
        removed: 0,
        clipped_lines: 0,
        full: None,
    };
    let done = GitDone::FileDiff { from, to, patch: Box::new(patch) };
    hub.update(cx, |hub, cx| hub.git_done(request, GitOutcome::Done(done), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-left-out-0").is_none(), "the row gave way");
    assert!(cx.debug_bounds("review-line-0-0-0").is_some(), "to its lines");
    assert!(cx.debug_bounds("review-line-0-0-1").is_some(), "every one");

    sent.borrow_mut().clear();
    click(cx, "review-whole-1");
    let request = sent.borrow().iter().find_map(|m| match m {
        ClientMsg::Git { request, op: GitOp::FileDiff { .. }, .. } => Some(*request),
        _ => None,
    });
    let request = request.expect("the other asked whole");
    let failed = GitOutcome::Failed { said: "fatal: bad object".to_owned() };
    hub.update(cx, |hub, cx| hub.git_done(request, failed, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-whole-said-1").is_some(), "why it could not be read");
    assert!(cx.debug_bounds("review-whole-1").is_some(), "and the press again");
    assert!(cx.debug_bounds("review-left-out-1").is_some(), "its row stays");
}

/// The keyboard reads a review down: ↓ and j walk the changes in the diff's order (the source
/// first), ⌘Y keeps the change it stands on and steps on, ⇧J goes to the next file's head, and
/// `v` marks that file viewed and folds it.
#[gpui::test]
fn keys_walk_the_changes_and_keep_them(cx: &mut TestAppContext) {
    use gpui::Focusable as _;
    let (view, _hub, sent, cx) = tile(cx, 1200.0);
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    cx.simulate_keystrokes("down j");
    cx.simulate_keystrokes("cmd-y");
    assert_eq!(
        intents(&sent),
        [Intent::Keep(slopty_proto::thread::wire::Pick {
            path: "src/lib.rs".to_owned(),
            from: Some("src/lib.rs@old".to_owned()),
            stamp: Some("src/lib.rs@new".to_owned()),
            hunks: vec![1],
            old_path: None,
        })],
        "the source's second change, kept by its key"
    );
    // On to the lock's one change: its file keeps whole.
    cx.simulate_keystrokes("cmd-y");
    let kept: Vec<_> = intents(&sent)
        .into_iter()
        .filter_map(|i| match i {
            Intent::Keep(p) => Some((p.path, p.hunks)),
            _ => None,
        })
        .collect();
    assert_eq!(kept.last(), Some(&("Cargo.lock".to_owned(), Vec::new())), "{kept:?}");

    cx.simulate_keystrokes("shift-j");
    assert!(cx.debug_bounds("review-line-2-0-0").is_some(), "the test file is open");
    cx.simulate_keystrokes("v");
    assert!(cx.debug_bounds("review-line-2-0-0").is_none(), "viewed, it folds to its head");
    assert!(view.read_with(cx, |v, _| v.viewed_count()) == 1, "and is marked viewed");
}

/// A bare key typed into a comment's field is a letter, not a step.
#[gpui::test]
fn a_comment_s_field_keeps_its_letters(cx: &mut TestAppContext) {
    let (_view, _hub, sent, cx) = tile(cx, 800.0);
    click(cx, "review-line-1-0-2");
    cx.simulate_input("jvc");
    cx.simulate_keystrokes("enter");
    assert!(intents(&sent).is_empty(), "no key acted");
    assert!(cx.debug_bounds("review-comment-0").is_some(), "the comment kept its words");
}

/// On a tile too narrow for the list of files, the scope bar's Files opens them as a menu,
/// and a file picked there is where the keyboard stands.
#[gpui::test]
fn a_narrow_tile_goes_to_a_file_from_its_menu(cx: &mut TestAppContext) {
    let (view, _hub, _sent, cx) = tile(cx, 600.0);
    assert!(cx.debug_bounds("review-files").is_none(), "no list beside the diff");
    click(cx, "review-files-button");
    assert!(cx.debug_bounds("review-files-file-2").is_some(), "the files, as a menu");
    click(cx, "review-files-file-2");
    assert_eq!(view.read_with(cx, |v, _| v.cursor_file()), Some(2));
}

/// What a file's head and its empty row say of a rename, a change of mode, a picture and a file
/// too large: never "Binary file" or "No lines to show" for what has a name.
#[test]
fn a_rename_a_mode_and_a_large_file_are_said_in_words() {
    use slopty_proto::thread::wire::{FileKind, Modes};

    use super::view::sides::{bare_words, head_words, mode_words};

    let exec = |from, to| mode_words(Modes { from, to });
    assert_eq!(exec(0o100_644, 0o100_755), "Made executable");
    assert_eq!(exec(0o100_755, 0o100_644), "No longer executable");
    assert_eq!(exec(0o100_644, 0o120_000), "Now a symbolic link");
    assert_eq!(exec(0o120_000, 0o100_755), "No longer a symbolic link");
    assert_eq!(exec(0o100_644, 0o160_000), "Mode 100644 \u{2192} 160000");

    let mut renamed = file("src/new.rs", &[], 0, 0);
    renamed.patch.hunks.clear();
    renamed.old_path = Some("src/old.rs".to_owned());
    assert_eq!(head_words(&renamed).as_deref(), Some("Renamed from old.rs"));
    assert_eq!(bare_words(&renamed), "No change to its lines", "the head says the move");
    renamed.old_path = Some("lib/old.rs".to_owned());
    renamed.modes = Some(Modes { from: 0o100_644, to: 0o100_755 });
    assert_eq!(
        head_words(&renamed).as_deref(),
        Some("Moved from lib/old.rs \u{b7} Made executable")
    );
    assert_eq!(bare_words(&renamed), "No change to its lines", "nor the mode again");

    let mut added = file("a.png", &[], 0, 0);
    added.from = None;
    assert_eq!(head_words(&added).as_deref(), Some("Added"));
    added.kind = FileKind::TooLarge { bytes: 6 << 20 };
    assert_eq!(bare_words(&added), "6.0 MB, too large to show its changes here");
    added.kind = FileKind::Binary;
    assert_eq!(bare_words(&added), "Binary file");
}

/// A picture shows its two sides, each asked of the worker by its blob once, the first time
/// its row is drawn: one that came is drawn, one that could not be read says why. A text file
/// too large to cut says its size and opens whole in a tile of its own once the repository's
/// root is known.
#[gpui::test]
fn a_picture_shows_both_sides_and_a_large_file_opens_whole(cx: &mut TestAppContext) {
    use slopty_proto::git::{GitDone, GitOp, GitOutcome, GitStatus};
    use slopty_proto::thread::wire::FileKind;

    /// A one-pixel PNG.
    const PIXEL: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    let (view, hub, sent, cx) = tile_in(cx, 1200.0, Some("/w/repo"));
    let heard: Rc<RefCell<Vec<ReviewEvent>>> = Rc::default();
    let into = Rc::clone(&heard);
    cx.update(|_w, cx| {
        cx.subscribe(&view, move |_view, event: &ReviewEvent, _cx| {
            if matches!(event, ReviewEvent::OpenFile { .. }) {
                into.borrow_mut().push(event.clone());
            }
        })
        .detach();
    });
    let thread = sent
        .borrow()
        .iter()
        .find_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Review { thread, .. }) => Some(*thread),
            _ => None,
        })
        .expect("the review was asked");
    let bare = |path: &str, kind| {
        let mut file = file(path, &[], 0, 0);
        file.patch.hunks.clear();
        file.kind = kind;
        file
    };
    let review = Review {
        scope: ReviewScope::Turn(TurnId(1)),
        from: None,
        to: None,
        files: vec![
            bare("logo.png", FileKind::Image { bytes: 2_048 }),
            bare("data.json", FileKind::TooLarge { bytes: 6 << 20 }),
        ],
        absent: None,
    };
    sent.borrow_mut().clear();
    hub.update(cx, |hub, cx| hub.frame(thread, ThreadFrame::Review(Box::new(review)), cx));
    cx.run_until_parked();
    let blobs = |sent: &Sent| -> Vec<(u64, String)> {
        sent.borrow()
            .iter()
            .filter_map(|m| match m {
                ClientMsg::Git { request, op: GitOp::Blob { blob }, .. } => {
                    Some((*request, blob.clone()))
                }
                _ => None,
            })
            .collect()
    };
    let asked = blobs(&sent);
    let names: Vec<&str> = asked.iter().map(|(_, b)| b.as_str()).collect();
    assert_eq!(names, ["logo.png@old", "logo.png@new"], "each side once");
    assert!(cx.debug_bounds("review-picture-before-0").is_some());
    assert!(cx.debug_bounds("review-picture-after-0").is_some());

    let (old, new) = (asked[0].0, asked[1].0);
    let came = GitDone::Blob { blob: "logo.png@new".to_owned(), bytes: PIXEL.to_vec() };
    hub.update(cx, |hub, cx| hub.git_done(new, GitOutcome::Done(came), cx));
    let failed = GitOutcome::Failed { said: "fatal: bad object".to_owned() };
    hub.update(cx, |hub, cx| hub.git_done(old, failed, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(blobs(&sent).len() == 2, "nothing asked again");
    let pictures = view.read_with(cx, |v, _| v.pictures_held());
    assert_eq!(pictures, ["logo.png@new"], "the side that came is decoded");

    assert!(cx.debug_bounds("review-too-large-1").is_some(), "the large file says its size");
    assert!(cx.debug_bounds("review-open-whole-1").is_none(), "no root, no open");
    let status = sent.borrow().iter().find_map(|m| match m {
        ClientMsg::Git { request, op: GitOp::Status, .. } => Some(*request),
        _ => None,
    });
    let root = GitStatus {
        root: "/r".to_owned(),
        forge: None,
        branch: Some("main".to_owned()),
        head: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        files: Vec::new(),
        more: 0,
    };
    let status = status.unwrap_or_else(|| {
        hub.update(cx, |hub, cx| hub.git_op("/w/repo", GitOp::Status, cx)).expect("asked")
    });
    let done = GitOutcome::Done(GitDone::Status(Box::new(root)));
    hub.update(cx, |hub, cx| hub.git_done(status, done, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    click(cx, "review-open-whole-1");
    assert_eq!(*heard.borrow(), [ReviewEvent::OpenFile { path: "/r/data.json".to_owned() }]);
}

/// The branch's pull request's review threads hang in the diff: one on a line on show under
/// that line, one on a line not on show and one on code changed since at their file's end,
/// and a reviewer's words over the whole nowhere here. The person's own comments go to the
/// pull request as their review, anchored at its head; once the forge took them they go and
/// what was posted is said, and a post turned down keeps them and says why.
#[gpui::test]
fn forge_threads_hang_on_their_lines_and_comments_post_as_a_review(cx: &mut TestAppContext) {
    use slopty_proto::git::{
        GitDone, GitOp, GitOutcome, LineSide, PullComments, PullNote, PullStatus, PullThread,
        ReviewNote, ReviewVerdict,
    };

    let (view, hub, sent, cx) = tile(cx, 1200.0);
    let asked = |sent: &Sent, pick: fn(&GitOp) -> bool| {
        sent.borrow().iter().rev().find_map(|m| match m {
            ClientMsg::Git { request, op, .. } if pick(op) => Some((*request, op.clone())),
            _ => None,
        })
    };
    let (read, _) = asked(&sent, |op| matches!(op, GitOp::PullStatus)).expect("the pull asked");
    let pull = PullStatus {
        forge: slopty_proto::git::Forge::GitHub,
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
    hub.update(cx, |hub, cx| hub.git_done(read, done, cx));
    cx.run_until_parked();
    let (read, _) = asked(&sent, |op| matches!(op, GitOp::PullComments { number: 12 }))
        .expect("an open pull request's threads are read");
    let thread = |line: u32, outdated: bool, path: Option<&str>| PullThread {
        path: path.map(str::to_owned),
        line: Some(line),
        outdated,
        url: Some(format!("https://github.com/o/r/pull/12#r{line}")),
        notes: vec![PullNote { author: "ana".to_owned(), body: format!("About line {line}") }],
    };
    let comments = PullComments {
        number: 12,
        threads: vec![
            thread(11, false, Some("src/lib.rs")),
            thread(99, false, Some("src/lib.rs")),
            thread(11, true, Some("src/lib.rs")),
            thread(1, false, None),
        ],
        more: 0,
    };
    let done = GitOutcome::Done(GitDone::PullComments(Box::new(comments)));
    hub.update(cx, |hub, cx| hub.git_done(read, done, cx));
    cx.run_until_parked();
    let top = |cx: &mut VisualTestContext, s: &'static str| {
        cx.debug_bounds(s).unwrap_or_else(|| panic!("{s} drawn")).origin.y
    };
    // `src/lib.rs` is the review's second file; its first hunk's third line is new line 11.
    let under = top(cx, "review-forge-0");
    assert!(top(cx, "review-line-1-0-2") < under && under < top(cx, "review-line-1-0-3"));
    let end = top(cx, "review-line-1-1-1");
    assert!(top(cx, "review-forge-1") > end && top(cx, "review-forge-2") > end, "at its end");
    assert!(cx.debug_bounds("review-forge-3").is_none(), "words over the whole are not here");
    assert!(cx.debug_bounds("review-post").is_none(), "nothing of the person's to post");

    let line = cx.debug_bounds("review-line-1-1-1").expect("drawn").center();
    cx.simulate_mouse_down(line, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(line, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    cx.simulate_input("Log at debug");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    click(cx, "review-post");
    click(cx, "review-post-changes");
    let (request, op) = asked(&sent, |op| matches!(op, GitOp::PullReview { .. })).expect("posted");
    let note = ReviewNote {
        path: "src/lib.rs".to_owned(),
        line: 41,
        side: LineSide::New,
        body: "Log at debug".to_owned(),
    };
    assert_eq!(
        op,
        GitOp::PullReview {
            number: 12,
            verdict: ReviewVerdict::RequestChanges,
            body: String::new(),
            notes: vec![note],
            head: Some("abc".to_owned()),
        }
    );
    let failed = GitOutcome::Failed { said: "pull request moved past abc".to_owned() };
    hub.update(cx, |hub, cx| hub.git_done(request, failed, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-comment-0").is_some(), "a post turned down keeps them");
    let said = view.read_with(cx, |v, _| v.came_words());
    assert_eq!(said.as_deref(), Some("Review not posted to #12: pull request moved past abc"));

    click(cx, "review-post");
    click(cx, "review-post-comment");
    let (request, _) = asked(&sent, |op| matches!(op, GitOp::PullReview { .. })).expect("again");
    let took = GitDone::PullReviewed { url: None, posted: 1 };
    hub.update(cx, |hub, cx| hub.git_done(request, GitOutcome::Done(took), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-comment-0").is_none(), "taken, they go");
    let said = view.read_with(cx, |v, _| v.came_words());
    assert_eq!(said.as_deref(), Some("Posted 1 comment to #12"));
}

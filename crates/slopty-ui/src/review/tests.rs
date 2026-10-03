//! The review tile in a headless window over a hub that holds a recorded review: the order of
//! its files, its two layouts, and what keeping, commenting and marking it reviewed send.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use slopty_proto::ClientMsg;
use slopty_proto::thread::detail::Hunk;
use slopty_proto::thread::wire::{
    FileDiff, Intent, IntentDone, Outcome, Review, ReviewScope, ThreadFrame, ThreadRequest,
};
use slopty_proto::thread::{Cursor, Delivery, Patch, TurnId};
use slopty_theme::Theme;

use super::view::ReviewView;
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
/// one message, `path L<n>: body`, and none wait after.
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
            text: "src/lib.rs L11: Why new?".to_owned(),
            delivery: Delivery::Steer,
            attachments: vec![]
        }]
    );
    assert!(cx.debug_bounds("review-comment-0").is_none());
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

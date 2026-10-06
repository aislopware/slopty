//! The palette's Threads section in the headless workspace: one ask of every linked worker
//! once the field rests, none of a worker with no link, an answer for words no longer in the
//! field dropped, and the line the person is on kept as the workers' answers come in.

use slopty_proto::thread::wire::{ItemHit, ThreadHit, ThreadHits, ThreadRequest};
use slopty_proto::thread::{ItemId, ThreadId, TurnId};

use super::*;
use crate::conversation::thread::find::ASK_AFTER;
use crate::palette::{PaletteRun, Section};

fn open_palette(cx: &mut VisualTestContext) {
    cx.dispatch_action(OpenPalette);
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_some(), "the palette is up");
}

/// The words each thread search `fake` was sent asked for, in order.
fn asked(fake: &mut Fake) -> Vec<String> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Search { query, .. }) => Some(query),
            _ => None,
        })
        .collect()
}

/// Threads `named` with a match for `query`, each at turn 3.
fn hits(query: &str, named: &[&str]) -> ThreadHits {
    let threads = named
        .iter()
        .map(|name| ThreadHit {
            thread: ThreadId::derived(&[name]),
            hits: vec![ItemHit {
                item: ItemId(format!("{name}.a")),
                turn: TurnId(3),
                said: ItemHit::AGENT.to_owned(),
                text: format!("the parser in {name}\nmoved on"),
                spans: Vec::new(),
                cut_before: true,
                cut_after: false,
                at_ms: WallMs::ZERO,
            }],
            more: 0,
        })
        .collect();
    ThreadHits { query: query.to_owned(), threads, more: 0 }
}

fn answer(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    key: WorkerKey,
    hits: ThreadHits,
) {
    view.update_in(cx, |v, _w, cx| v.thread_hits(key, hits, cx));
    cx.run_until_parked();
}

/// The Threads section's lines: each one's thread, and what it shows of where it was said.
fn thread_lines(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Vec<(ThreadId, String)> {
    view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("open");
        palette
            .read(cx)
            .matches()
            .into_iter()
            .filter(|l| l.section == Section::Threads)
            .filter_map(|l| match l.run {
                PaletteRun::Thread { thread, .. } => Some((thread, l.a11y_label())),
                _ => None,
            })
            .collect()
    })
}

/// The thread of the line the person is on, or its label when it is no thread's.
fn on(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("open");
        let palette = palette.read(cx);
        let line = palette.chosen().expect("a line is chosen");
        match line.run {
            PaletteRun::Thread { thread, .. } => format!("{thread:?}"),
            _ => line.label.clone(),
        }
    })
}

#[gpui::test]
fn the_palette_asks_every_linked_workers_threads_once_the_field_rests(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let mut laptop = connect(&view, cx, 2, "laptop");
    let mut away = connect(&view, cx, 3, "away");
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(away.key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
    open_palette(cx);

    // A burst of keys is one ask, made once it rests, of each worker with a link.
    cx.simulate_input("n");
    cx.simulate_input("e");
    cx.executor().advance_clock(ASK_AFTER.div_f32(2.0));
    cx.simulate_input("w");
    cx.run_until_parked();
    assert_eq!(asked(&mut studio), Vec::<String>::new(), "nothing before the field rests");
    cx.executor().advance_clock(ASK_AFTER);
    cx.run_until_parked();
    assert_eq!(asked(&mut studio), ["new"]);
    assert_eq!(asked(&mut laptop), ["new"]);
    assert_eq!(asked(&mut away), Vec::<String>::new(), "a worker with no link is not asked");
    // The words are not asked again while they stay the field's.
    cx.executor().advance_clock(ASK_AFTER.saturating_mul(4));
    cx.run_until_parked();
    assert_eq!(asked(&mut studio), Vec::<String>::new());

    // The person moves down the list's own lines before any worker answers.
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    let kept = on(&view, cx);

    // An answer for words the field no longer says is never shown.
    answer(&view, cx, studio.key, hits("ne", &["stale"]));
    assert_eq!(thread_lines(&view, cx), Vec::new(), "an answer for other words is dropped");

    // The Threads section fills in under the line the person is on, which stays chosen.
    answer(&view, cx, studio.key, hits("new", &["s1", "s2"]));
    let lines = thread_lines(&view, cx);
    let threads: Vec<ThreadId> = lines.iter().map(|(t, _)| *t).collect();
    assert_eq!(threads, [ThreadId::derived(&["s1"]), ThreadId::derived(&["s2"])], "{lines:#?}");
    let (_, label) = lines.first().expect("a line");
    assert!(
        label.contains("…the parser in s1 moved on"),
        "where it was said, on one line: {label}"
    );
    assert!(label.contains("studio"), "its worker, with two linked: {label}");
    assert_eq!(on(&view, cx), kept, "the line the person is on stays chosen");

    // On a thread's line, a later answer that lands above it moves the line, not the choice.
    let down_to_s2 = view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("open");
        let matches = palette.read(cx).matches();
        let s2 = matches
            .iter()
            .position(|l| matches!(l.run, PaletteRun::Thread { thread, .. } if thread == ThreadId::derived(&["s2"])))
            .expect("s2 listed");
        let at = matches.iter().position(|l| l.label == kept).expect("kept listed");
        s2.saturating_sub(at)
    });
    for _ in 0..down_to_s2 {
        cx.simulate_keystrokes("down");
    }
    cx.run_until_parked();
    assert_eq!(on(&view, cx), format!("{:?}", ThreadId::derived(&["s2"])));
    answer(&view, cx, laptop.key, hits("new", &["l1"]));
    let threads: Vec<ThreadId> = thread_lines(&view, cx).into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        threads,
        [ThreadId::derived(&["s1"]), ThreadId::derived(&["l1"]), ThreadId::derived(&["s2"])],
        "each worker's best first, in turn"
    );
    assert_eq!(on(&view, cx), format!("{:?}", ThreadId::derived(&["s2"])), "still on s2");

    // New words drop the last answers at once, and their ask waits for the field to rest.
    cx.simulate_input(" t");
    cx.run_until_parked();
    assert_eq!(thread_lines(&view, cx), Vec::new());
    cx.executor().advance_clock(ASK_AFTER);
    cx.run_until_parked();
    assert_eq!(asked(&mut studio), ["new t"]);
    answer(&view, cx, laptop.key, hits("new", &["late"]));
    assert_eq!(thread_lines(&view, cx), Vec::new(), "a late answer for the last words is dropped");
}

/// A field searching the commands alone (`>`), or of one character, asks no worker.
#[gpui::test]
fn the_palette_asks_no_threads_for_commands_or_a_single_character(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    open_palette(cx);
    cx.simulate_input("p");
    cx.executor().advance_clock(ASK_AFTER.saturating_mul(2));
    cx.run_until_parked();
    assert_eq!(asked(&mut studio), Vec::<String>::new(), "one character");
    cx.simulate_keystrokes("backspace");
    cx.simulate_input("> new");
    cx.executor().advance_clock(ASK_AFTER.saturating_mul(2));
    cx.run_until_parked();
    assert_eq!(asked(&mut studio), Vec::<String>::new(), "the commands alone");
}

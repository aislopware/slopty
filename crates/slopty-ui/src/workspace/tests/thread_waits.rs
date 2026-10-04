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

/// A Codex thread waiting on an approval marks its navigator row, wears the header's pill,
/// counts on the bell and the Dock, lists under *Needs you* with what it asks, and its row there
/// opens its tile. Answered, every one of them lets go.
#[gpui::test]
fn a_thread_with_no_terminal_that_waits_says_so_across_the_chrome(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let row = asking(None);
    let thread = row.id;
    let tile = arrives(&view, cx, &studio, ItemKind::Thread { thread }, 1);
    let file = arrives(&view, cx, &studio, ItemKind::File { path: "/w/a.txt".to_owned() }, 2);
    table(&view, cx, key, vec![row.clone()]);

    view.update(cx, |v, _| {
        let item = v.item(tile).cloned().expect("the tile");
        assert_eq!(v.tile_status(tile, &item), Some(Status::NeedsYou), "the navigator's glyph");
        assert_eq!((v.needs_you_count(), v.needs_you_on(key)), (1, 1), "the Dock's count");
        assert_eq!(v.bell_count(), 1, "the bell's");
        let look = v.attention_look();
        let [asks] = look.asking.as_slice() else { panic!("one asks: {look:?}") };
        assert_eq!(asks.route.about, attention::About::Thread(thread), "a note of its own");
        assert_eq!(asks.route.item, Some(tile.item), "that leads to its tile");
        assert_eq!(asks.body, "Run cargo test", "what it asks, said plainly");
    });
    let pill = leak(format!("agent-{}", tile.item.as_uuid()));
    assert!(cx.debug_bounds(pill).is_some(), "the header's pill");

    view.update_in(cx, |v, _w, cx| v.focus_tile(file, cx));
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(file));
    let line = leak(format!("nav-waiting-{thread}"));
    let at = cx.debug_bounds(line).expect("a row of its own under Needs you");
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
        assert_eq!((v.needs_you_count(), v.bell_count()), (0, 0), "answered, it lets go");
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

/// A thread of `rows`' kind with an id of its own.
fn another(mut row: ThreadRow) -> ThreadRow {
    row.id = slopty_proto::thread::ThreadId::new();
    row
}

/// The thread tiles `drained` asked the worker to add.
fn thread_tiles_added(drained: &[ClientMsg]) -> Vec<slopty_proto::thread::ThreadId> {
    drained
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(Item { kind: ItemKind::Thread { thread }, .. })) => {
                Some(*thread)
            }
            _ => None,
        })
        .collect()
}

/// ⌘⇧A and the navigator's *Needs you* list a waiting thread as they list a waiting terminal:
/// on the needs-you rung in reading order with the terminals, those with no tile after them,
/// then a thread that failed on the rung of what failed. Each step opens its thread: its tile,
/// or a new one on its worker.
#[gpui::test]
fn the_ladder_and_needs_you_list_a_waiting_thread_by_its_rung(cx: &mut TestAppContext) {
    use super::super::agents::Step;
    use super::super::faces::ThreadWait;

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let session = SessionId::new();
    let shell = opens(&view, cx, &studio, session, studio.me, 1);
    let tiled = another(asking(None));
    let tile = arrives(&view, cx, &studio, ItemKind::Thread { thread: tiled.id }, 2);
    let untiled = another(asking(None));
    let mut failed = another(asking(None));
    failed.requests.clear();
    failed.status.phase = slopty_proto::thread::Phase::Failed;
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    table(&view, cx, key, vec![tiled.clone(), untiled.clone(), failed.clone()]);

    let ladder = view.read_with(cx, |v, _| v.attention_ladder());
    let said: Vec<(Option<TileRef>, Option<slopty_proto::thread::ThreadId>)> = ladder
        .iter()
        .map(|step| match step {
            Step::Session(w) => (w.tile, None),
            Step::Thread(w) => (w.tile, Some(w.thread)),
        })
        .collect();
    assert_eq!(
        said,
        [
            (Some(shell), None),
            (Some(tile), Some(tiled.id)),
            (None, Some(untiled.id)),
            (None, Some(failed.id)),
        ],
        "the waiting terminal and thread in reading order, the untiled one, then the failed"
    );

    // *Needs you* lists every thread that waits: the one in view and the one with no tile.
    assert!(cx.debug_bounds("nav-needs-you").is_some(), "the section");
    assert!(cx.debug_bounds(leak(format!("nav-waiting-{}", tiled.id))).is_some(), "in view");
    let row = leak(format!("nav-waiting-{}", untiled.id));
    let words = leak(format!("nav-waiting-words-{}", untiled.id));
    assert!(cx.debug_bounds(words).is_some(), "what it asks under the heading");
    studio.drain();
    let at = cx.debug_bounds(row).expect("the untiled thread's row").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(thread_tiles_added(&studio.drain()), [untiled.id], "its row opens it");

    // ⌘⇧A from the shell walks the waiting tiles in reading order, the thread its row opened
    // among them now, then opens the failed thread.
    let ladder = view.read_with(cx, |v, _| v.attention_ladder());
    let opened = view.read_with(cx, |v, _| v.tile_of_thread(untiled.id)).expect("its new tile");
    let tiles: Vec<Option<TileRef>> = ladder.iter().map(|s| s.tile()).collect();
    assert_eq!(tiles.first(), Some(&Some(shell)), "{ladder:?}");
    assert!(tiles.contains(&Some(tile)) && tiles.contains(&Some(opened)), "{ladder:?}");
    assert_eq!(
        ladder.last(),
        Some(&Step::Thread(ThreadWait { worker: key, thread: failed.id, tile: None }))
    );
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    for step in ladder.iter().skip(1).take(2) {
        cx.simulate_keystrokes("cmd-shift-a");
        cx.run_until_parked();
        assert_eq!(focused(&view, cx), step.tile(), "{ladder:?}");
    }
    studio.drain();
    cx.simulate_keystrokes("cmd-shift-a");
    cx.run_until_parked();
    assert_eq!(thread_tiles_added(&studio.drain()), [failed.id], "the failed thread, opened");
}

/// The server's ladder speaks for the threads of a worker this client has no link to, as its
/// terminals' agents do: a waiting thread counts there and lists under *Needs you*. A thread its
/// terminal's agent already speaks for counts once, by the terminal; a worker reached both
/// ways counts its threads once, by its own link while that is up and by the server's word
/// once it drops; and forgetting the server forgets its word.
#[gpui::test]
fn a_thread_the_server_ranks_counts_once_however_its_worker_is_reached(cx: &mut TestAppContext) {
    use slopty_core::WorkerId;
    use slopty_proto::orchestration::TermRef;
    use slopty_proto::thread::attention::{Ladder, Ranked, Rung, Standing, ThreadAt};

    use super::super::projects::worker_key;

    let (view, cx) = workspace(cx);
    let (studio_id, laptop_id) = (WorkerId::new(), WorkerId::new());
    let studio = connect(&view, cx, worker_key(studio_id).value(), "studio");
    let key = studio.key;
    let laptop = worker_key(laptop_id);
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        v.add_worker(laptop, "laptop".into(), cx);
    });
    let own = asking(None);
    table(&view, cx, key, vec![own.clone()]);
    assert_eq!(view.read_with(cx, |v, _| v.needs_you_count()), 1, "the link's own word");

    let (alone, on_terminal, also_on_terminal, terminal) = (
        slopty_proto::thread::ThreadId::new(),
        slopty_proto::thread::ThreadId::new(),
        slopty_proto::thread::ThreadId::new(),
        SessionId::new(),
    );
    let ranked = |worker: WorkerId, thread, terminal| Ranked {
        at: ThreadAt { worker, thread },
        rung: Rung::NeedsYou,
        since_ms: WallMs::from_millis(1),
        terminal,
    };
    // Two threads in one Claude Code terminal both wait: its agent speaks for both, once.
    let threads = vec![
        ranked(studio_id, own.id, None),
        ranked(laptop_id, alone, None),
        ranked(laptop_id, on_terminal, Some(terminal)),
        ranked(laptop_id, also_on_terminal, Some(terminal)),
    ];
    let tile = Standing::of(threads.iter().filter(|r| r.terminal == Some(terminal)));
    let ladder = Ladder {
        threads,
        tiles: vec![(TermRef { worker: laptop_id, session: terminal }, tile)],
        ..Ladder::default()
    };
    view.update_in(cx, |v, _w, cx| {
        v.server_agent_event(laptop, blocked(terminal), cx);
        v.server_ladder(&ladder, cx);
    });
    cx.run_until_parked();
    view.update(cx, |v, _| {
        assert_eq!(v.needs_you_on(key), 1, "the studio's thread once, though both say it");
        assert_eq!(v.needs_you_on(laptop), 2, "the laptop's thread, and its terminal once");
        assert_eq!(v.bell_count(), 3, "the bell's");
    });
    assert!(cx.debug_bounds(leak(format!("nav-waiting-{alone}"))).is_some(), "a row of its own");
    for on_it in [on_terminal, also_on_terminal] {
        assert!(cx.debug_bounds(leak(format!("nav-waiting-{on_it}"))).is_none(), "its terminal's");
    }

    view.update_in(cx, |v, _w, cx| v.disconnect_worker(key, WorkerStatus::Connecting, cx));
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| v.needs_you_on(key)),
        1,
        "its link gone, the server's word stands in"
    );
    // Answered meanwhile: the server says so, and the link's last table, stale, says nothing.
    let mut answered = ladder;
    answered.threads[0].rung = Rung::Working;
    view.update_in(cx, |v, _w, cx| v.server_ladder(&answered, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.needs_you_on(key)), 0, "not the stale table's word");

    view.update_in(cx, |v, _w, cx| v.forget_server_agents(None, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.needs_you_count()), 0, "no word left to go on");
}

/// The answer `drained` sent a thread, if any.
fn answer_sent(drained: &[ClientMsg]) -> Vec<slopty_proto::thread::wire::Intent> {
    use slopty_proto::thread::wire::{Intent, ThreadRequest};
    drained
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent {
                intent: i @ Intent::Answer { .. }, ..
            }) => Some(i.clone()),
            _ => None,
        })
        .collect()
}

/// A thread with no terminal that waits on a yes or no it offers plainly is answered from its
/// row's "Allow" and "Deny" under *Needs you*, and its note carries the request for the note's
/// buttons. The answer is the request's own plain allow, never its standing grant, sent through the
/// worker's thread hub once: the row takes its buttons back until the table moves. A request
/// with no plain answer offers none.
#[gpui::test]
fn a_thread_s_yes_or_no_is_answered_from_its_row_and_its_note(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::Intent;
    use slopty_proto::thread::{Choice, Effect};
    let choice = |id: &str, effect, scope: Option<&str>| Choice {
        id: id.to_owned(),
        label: id.to_owned(),
        effect,
        scope: scope.map(str::to_owned),
        stops: false,
    };
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let mut row = asking(None);
    let thread = row.id;
    if let Some(card) = row.requests.first_mut() {
        card.options = vec![
            choice("always", Effect::Allow, Some("cargo in this folder")),
            choice("accept", Effect::Allow, None),
            choice("decline", Effect::Deny, None),
        ];
    }
    table(&view, cx, key, vec![row.clone()]);
    let approval = view.update(cx, |v, _| v.attention_look().asking[0].approval.clone());
    assert_eq!(approval.as_deref(), Some("ask-1"), "its note answers the request");

    cx.run_until_parked();
    studio.drain();
    let allow = cx.debug_bounds(leak(format!("nav-allow-{thread}"))).expect("Allow on its row");
    cx.simulate_click(allow.center(), Modifiers::none());
    cx.run_until_parked();
    let sent = answer_sent(&studio.drain());
    assert_eq!(
        sent,
        [Intent::Answer {
            ask: AskId("ask-1".to_owned()),
            choice: "accept".to_owned(),
            message: None
        }],
        "the plain allow, once"
    );
    assert!(
        cx.debug_bounds(leak(format!("nav-allow-{thread}"))).is_none(),
        "answered here, the row waits for its worker"
    );
    let again = view.update_in(cx, |v, _w, cx| {
        v.answer_thread(key, thread, &AskId("ask-1".to_owned()), false, cx)
    });
    assert!(!again, "once");

    let mut later = row.clone();
    if let Some(card) = later.requests.first_mut() {
        card.id = AskId("ask-2".to_owned());
    }
    table(&view, cx, key, vec![later]);
    studio.drain();
    let tap = slopty_platform::notify::Tap {
        id: format!("thread-{thread}"),
        info: [
            ("worker".to_owned(), key.value().to_string()),
            ("thread".to_owned(), thread.to_string()),
            ("ask".to_owned(), "ask-2".to_owned()),
        ]
        .into(),
        action: Some(slopty_platform::notify::DENY.to_owned()),
    };
    view.update_in(cx, |v, _w, cx| v.open_notification(&tap, cx));
    cx.run_until_parked();
    let sent = answer_sent(&studio.drain());
    assert_eq!(
        sent,
        [Intent::Answer {
            ask: AskId("ask-2".to_owned()),
            choice: "decline".to_owned(),
            message: None
        }],
        "the note's Deny"
    );

    let mut bare = another(asking(None));
    if let Some(card) = bare.requests.first_mut() {
        card.options = vec![choice("always", Effect::Allow, Some("everything"))];
    }
    table(&view, cx, key, vec![bare]);
    let look = view.update(cx, |v, _| v.attention_look());
    assert_eq!(look.asking[0].approval, None, "no plain answer, no buttons");
}

/// A thread off screen that comes to need the person points the corner at it, with "Go", once
/// its worker's table says so; answered, the word goes.
#[gpui::test]
fn a_thread_off_screen_that_comes_to_need_you_is_pointed_at(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(600.0), px(500.0)));
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let asks = asking(None);
    let thread = asks.id;
    let mut calm = asks.clone();
    calm.requests.clear();
    calm.status.phase = slopty_proto::thread::Phase::Working;
    let tile = arrives(&view, cx, &studio, ItemKind::Thread { thread }, 1);
    let _shells = three_shells_from(&view, cx, &studio, 2);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.drawn.on_screen.borrow().contains(&tile.item)));
    let said = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.toast_text());

    table(&view, cx, key, vec![calm.clone()]);
    table(&view, cx, key, vec![asks]);
    let line = said(cx).expect("the corner points at it");
    assert!(line.ends_with("needs approval"), "{line}");
    table(&view, cx, key, vec![calm]);
    assert_eq!(said(cx), None, "answered, it goes");
}

/// Three shells opened after `first` on `fake`, each a column of its own.
fn three_shells_from(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    first: u64,
) -> Vec<TileRef> {
    (first..first.saturating_add(3))
        .map(|v| opens(view, cx, fake, SessionId::new(), fake.me, v))
        .collect()
}

/// What a waiting thread asks gives way to its answers: words longer than the navigator's row
/// shrink to an ellipsis, and "Allow" stays whole inside the row.
#[gpui::test]
fn long_words_never_push_a_row_s_answers_out_of_view(cx: &mut TestAppContext) {
    use slopty_proto::thread::{Choice, Effect};
    let choice = |id: &str, effect| Choice {
        id: id.to_owned(),
        label: id.to_owned(),
        effect,
        scope: None,
        stops: false,
    };
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let mut row = asking(None);
    let thread = row.id;
    if let Some(card) = row.requests.first_mut() {
        card.title = "Run `cargo nextest run --workspace --all-features --no-fail-fast`".to_owned();
        card.options = vec![choice("accept", Effect::Allow), choice("decline", Effect::Deny)];
    }
    table(&view, cx, key, vec![row]);
    cx.run_until_parked();

    let row = cx.debug_bounds(leak(format!("nav-waiting-{thread}"))).expect("its row");
    let allow = cx.debug_bounds(leak(format!("nav-allow-{thread}"))).expect("Allow on its row");
    assert!(allow.right() <= row.right(), "Allow {allow:?} inside its row {row:?}");
}

//! A thread driven over a protocol (Codex, pi, an ACP agent) has no terminal whose agent status
//! could say that it waits: its row in its worker's table says it to the chrome instead, and a
//! thread whose terminal's agent already says it is not counted twice.

use slopty_proto::thread::wire::{RequestCard, TableFrame, ThreadRow};
use slopty_proto::thread::{AskId, Cursor, Request, ThreadId};

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
        buttons: Vec::new(),
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

/// A Codex thread waiting on an approval marks its navigator row, ends its header with the glyph,
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
    let glyph = leak(format!("status-{}", tile.item.as_uuid()));
    assert!(cx.debug_bounds(glyph).is_some(), "the header's glyph");

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
    row.id = ThreadId::new();
    row
}

/// The thread tiles `drained` asked the worker to add.
fn thread_tiles_added(drained: &[ClientMsg]) -> Vec<ThreadId> {
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
    table(&view, cx, key, vec![tiled.clone(), untiled.clone(), failed.clone()]);
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    cx.run_until_parked();

    let ladder = view.read_with(cx, |v, _| v.attention_ladder());
    let said: Vec<(Option<TileRef>, Option<ThreadId>)> = ladder
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

    let (alone, on_terminal, also_on_terminal, terminal) =
        (ThreadId::new(), ThreadId::new(), ThreadId::new(), SessionId::new());
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
    view.update_in(cx, |v, _w, cx| v.server_ladder(&ladder, cx));
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
        text: None,
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

/// A thread at rest at `ms` with no request, its agent's turn over.
fn resting(ms: u64) -> ThreadRow {
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.id = ThreadId::new();
    state.meta.terminal = None;
    state.status.phase = slopty_proto::thread::Phase::Idle;
    let mut row = state.row(WallMs::from_millis(ms));
    row.requests.clear();
    row.to_review = false;
    row.title = format!("rested {ms}");
    row.updated_ms = WallMs::from_millis(ms);
    row
}

/// The threads at rest with no tile here are found again under the navigator's last fold,
/// "Earlier": folded at first, then the newest few, then every one, each a click from its
/// tile. A thread at work is never among them, and one with a tile leaves the fold.
#[gpui::test]
fn threads_at_rest_wait_under_the_earlier_fold(cx: &mut TestAppContext) {
    use crate::workspace::navigator::EARLIER_SHOWN;

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let rows: Vec<ThreadRow> = (1..=10_u64).map(|n| resting(n.saturating_mul(1000))).collect();
    let newest = rows.last().map(|r| r.id).expect("ten");
    let mut working = asking(None);
    working.requests.clear();
    working.status.phase = slopty_proto::thread::Phase::Working;
    let at_work = working.id;
    table(&view, cx, key, rows.iter().cloned().chain([working]).collect());
    let shown = |cx: &mut VisualTestContext, thread: ThreadId| {
        cx.debug_bounds(leak(format!("nav-thread-{thread}"))).is_some()
    };
    assert!(cx.debug_bounds("nav-earlier").is_some(), "the fold");
    assert!(!shown(cx, newest), "folded at first");
    assert!(shown(cx, at_work), "a thread at work is listed with its project, as before");

    let fold = cx.debug_bounds("nav-earlier").expect("the fold");
    cx.simulate_click(fold.center(), Modifiers::none());
    cx.run_until_parked();
    let listed = rows.iter().filter(|r| shown(cx, r.id)).count();
    assert_eq!(listed, EARLIER_SHOWN, "the newest few");
    assert!(shown(cx, newest));
    let more = cx.debug_bounds("nav-earlier-more").expect("the rest a click away");
    cx.simulate_click(more.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(rows.iter().filter(|r| shown(cx, r.id)).count(), rows.len(), "every one");

    let row = cx.debug_bounds(leak(format!("nav-thread-{newest}"))).expect("its row");
    cx.simulate_click(row.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.tile_of_thread(newest)).is_some(), "its tile opens");
    assert!(!shown(cx, newest), "and it leaves the fold");
}

/// A closed agent's tile stays on the "Reopen" list after its session ends, and comes back as
/// its thread's tile with its agent taken up again by the agent's own door: Claude Code by a
/// start of its session.
#[gpui::test]
fn a_closed_agent_tile_comes_back_as_its_thread_taken_up_again(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::{ThreadFrame, ThreadRequest};
    use slopty_proto::thread::{AgentId, Drive, Liveness};

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let session = SessionId::new();
    let _tile = opens(&view, cx, &studio, session, studio.me, 1);
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.agent = AgentId::named(AgentId::CLAUDE_CODE);
    state.meta.drive = Drive::named(Drive::OBSERVED);
    state.meta.native = "5f1c".to_owned();
    state.meta.terminal = Some(session);
    let thread = state.meta.id;
    table(&view, cx, key, vec![state.row(WallMs::ZERO)]);

    view.update_in(cx, |v, _w, cx| v.close_shell(session, cx));
    cx.run_until_parked();
    cx.executor().advance_clock(UNDO_CLOSE);
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.closed_lines().len()), 1, "still on the list");
    state.meta.terminal = None;
    state.status.liveness = Liveness::Exited { resumable: true };
    table(&view, cx, key, vec![state.row(WallMs::ZERO)]);
    studio.drain();

    view.update_in(cx, |v, _w, cx| v.take_back(None, cx));
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.tile_of_thread(thread)).is_some(), "its thread's tile");
    let hub = view.update(cx, |v, cx| v.thread_hub(key, cx));
    let frame =
        ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, state: Box::new(state) };
    hub.update(cx, |hub, cx| hub.frame(thread, frame, cx));
    cx.run_until_parked();
    let resumed: Vec<Vec<String>> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some(start.args),
            _ => None,
        })
        .collect();
    assert_eq!(resumed, [vec!["--resume".to_owned(), "5f1c".to_owned()]], "taken up again");
}

/// A turn of a thread with no terminal (Codex beside no TUI, pi, an ACP agent, a message's
/// runs) that ends while nobody looks is left to review as a terminal's agent's is: its row
/// under *To review* with what the agent last said, a count on the bell, a note while the app
/// is away and the unseen dot on its tile. Looking at its tile clears all of them; a short turn,
/// or one watched, earns none.
#[gpui::test]
fn a_threads_finished_turn_without_a_terminal_is_to_review(cx: &mut TestAppContext) {
    use slopty_proto::thread::{Phase, Status as Stands};

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = None;
    let mut row = state.row(WallMs::ZERO);
    let thread = row.id;
    let tile = arrives(&view, cx, &studio, ItemKind::Thread { thread }, 1);
    let file = arrives(&view, cx, &studio, ItemKind::File { path: "/w/a.txt".to_owned() }, 2);
    view.update_in(cx, |v, _w, cx| v.focus_tile(file, cx));
    let base = row.status.clone();
    let at = move |phase: Phase, ms: u64| Stands {
        phase,
        since_ms: WallMs::from_millis(ms),
        ..base.clone()
    };
    let turn = |row: &mut ThreadRow, cx: &mut VisualTestContext, from: u64, to: u64| {
        row.status = at(Phase::Working, from);
        let next = row.ended.map_or(1, |e| e.turn.0.saturating_add(1));
        row.ended = None;
        table(&view, cx, key, vec![row.clone()]);
        row.status = at(Phase::Done, to);
        row.last_line = Some("Fixed the flaky test".to_owned());
        row.ended = Some(slopty_proto::thread::wire::TurnEnded {
            turn: slopty_proto::thread::TurnId(next),
            at_ms: WallMs::from_millis(to),
            ran_ms: to.saturating_sub(from),
            answered: true,
        });
        table(&view, cx, key, vec![row.clone()]);
    };

    turn(&mut row, cx, 1_000, 61_000);
    view.update(cx, |v, _| {
        let review = v.to_review();
        assert!(
            matches!(review.as_slice(), [agents::Step::Thread(w)] if w.thread == thread),
            "a row under To review: {review:?}"
        );
        assert_eq!(v.bell_count(), 1, "the bell counts it");
        let look = v.attention_look();
        let [ended] = look.turns.as_slice() else { panic!("one turn: {look:?}") };
        assert_eq!(ended.route.about, attention::About::Thread(thread), "a note of its own");
        assert_eq!(ended.route.item, Some(tile.item), "that leads to its tile");
        let item = v.item(tile).cloned().expect("the tile");
        assert!(v.tile_marks(tile, &item).1, "its tile's unseen dot");
    });
    let line = leak(format!("nav-review-{thread}"));
    assert!(cx.debug_bounds(line).is_some(), "drawn under To review");

    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    view.update(cx, |v, _| {
        assert!(v.to_review().is_empty() && v.bell_count() == 0, "looked at: nothing left");
        let item = v.item(tile).cloned().expect("the tile");
        assert!(!v.tile_marks(tile, &item).1, "and no dot");
    });

    view.update_in(cx, |v, _w, cx| v.focus_tile(file, cx));
    turn(&mut row, cx, 70_000, 71_000);
    assert!(view.read_with(cx, |v, _| v.to_review().is_empty()), "a short turn earns nothing");
}

/// A thread at rest whose latest turn, `turn`, the agent answered after running `ran_ms`, and
/// which the person has seen through `seen`.
fn ended(turn: u32, ran_ms: u64, seen: u32) -> ThreadRow {
    use slopty_proto::thread::TurnId;
    use slopty_proto::thread::wire::TurnEnded;
    let mut row = resting(1_000);
    let at_ms = WallMs::from_millis(90_000);
    row.ended = Some(TurnEnded { turn: TurnId(turn), at_ms, ran_ms, answered: true });
    row.seen = TurnId(seen);
    row.last_line = Some("Fixed the flaky test".to_owned());
    row
}

/// The seen marks `drained` sent, by thread.
fn seen_sent(drained: &[ClientMsg]) -> Vec<(ThreadId, u32)> {
    use slopty_proto::thread::wire::{Intent, ThreadRequest};
    drained
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent {
                thread,
                intent: Intent::Seen { turn },
                ..
            }) => Some((*thread, turn.0)),
            _ => None,
        })
        .collect()
}

/// What is unread is the worker's word: a turn that ended while this app was not running is
/// to review once its table comes, with no note (whoever heard it end told of it), and a turn
/// read on another device leaves the bell and *To review* here as the row's seen mark moves.
#[gpui::test]
fn a_turn_read_on_another_device_leaves_the_bell(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let row = ended(3, 120_000, 2);
    let thread = row.id;
    table(&view, cx, key, vec![row.clone()]);
    view.update(cx, |v, _| {
        let review = v.to_review();
        assert!(
            matches!(review.as_slice(), [agents::Step::Thread(w)] if w.thread == thread),
            "unread from the first table: {review:?}"
        );
        assert_eq!(v.bell_count(), 1);
        assert!(v.attention_look().turns.is_empty(), "found, not heard ending: no note");
    });
    assert!(cx.debug_bounds(leak(format!("nav-review-{thread}"))).is_some());

    let seen = ThreadRow { seen: slopty_proto::thread::TurnId(3), ..row };
    table(&view, cx, key, vec![seen]);
    view.update(cx, |v, _| {
        assert!(
            v.to_review().is_empty() && v.bell_count() == 0,
            "read elsewhere: {:?}",
            v.to_review()
        );
    });
    assert!(cx.debug_bounds(leak(format!("nav-review-{thread}"))).is_none());
}

/// Looking at a thread's tile marks its latest turn seen on its worker, once, so every other
/// device reads it as read; the mark shows here at once, before the worker says it back. A turn
/// that ends while its tile is in front is seen as it ends.
#[gpui::test]
fn looking_at_a_thread_marks_it_seen_on_its_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let row = ended(4, 120_000, 0);
    let thread = row.id;
    let tile = arrives(&view, cx, &studio, ItemKind::Thread { thread }, 1);
    let file = arrives(&view, cx, &studio, ItemKind::File { path: "/w/a.txt".to_owned() }, 2);
    view.update_in(cx, |v, _w, cx| v.focus_tile(file, cx));
    table(&view, cx, key, vec![row.clone()]);
    assert_eq!(view.read_with(cx, |v, _| v.bell_count()), 1);
    studio.drain();

    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    assert_eq!(seen_sent(&studio.drain()), [(thread, 4)], "the mark goes to the worker");
    assert_eq!(view.read_with(cx, |v, _| v.bell_count()), 0, "and shows at once");
    view.update_in(cx, |v, _w, cx| v.focus_tile(file, cx));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    assert_eq!(seen_sent(&studio.drain()), [], "once");

    let next = ThreadRow {
        ended: ended(5, 120_000, 0).ended,
        seen: slopty_proto::thread::TurnId(4),
        ..row
    };
    table(&view, cx, key, vec![next]);
    assert_eq!(seen_sent(&studio.drain()), [(thread, 5)], "seen as it ends, in front");
    assert_eq!(view.read_with(cx, |v, _| v.bell_count()), 0);
}

/// A thread whose worker says its changes wait unkept stays under *To review* after its tile
/// was looked at, on the bell too, until the worker says they were kept; "Review next" opens
/// its review.
#[gpui::test]
fn a_thread_with_changes_unkept_stays_to_review(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = None;
    let mut row = state.row(WallMs::ZERO);
    row.to_review = true;
    let thread = row.id;
    let tile = arrives(&view, cx, &studio, ItemKind::Thread { thread }, 1);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    table(&view, cx, key, vec![row.clone()]);
    view.update(cx, |v, _| {
        let review = v.to_review();
        assert!(
            matches!(review.as_slice(), [agents::Step::Thread(w)] if w.thread == thread),
            "looked at, still to review: {review:?}"
        );
        assert_eq!(v.bell_count(), 1, "the bell counts it");
    });

    cx.simulate_keystrokes("cmd-shift-r");
    cx.run_until_parked();
    let asked = view.read_with(cx, |v, _| v.review_of(thread).is_some());
    assert!(asked, "Review next opened its review");

    row.to_review = false;
    table(&view, cx, key, vec![row]);
    assert!(view.read_with(cx, |v, _| v.to_review().is_empty()), "kept: nothing left");
}

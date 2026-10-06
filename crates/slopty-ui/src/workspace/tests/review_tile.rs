//! A thread's review opens as a tile of its own on the agent's worker, takes the keyboard,
//! hands its comments to the agent's draft, and lets the thread go once its tile is closed.

use slopty_core::WallMs;
use slopty_proto::thread::Cursor;
use slopty_proto::thread::wire::TableFrame;

use super::*;
use crate::conversation::thread::ThreadViewEvent;

/// The thread view's "Review" adds a review item on the agent's worker; once the registry has
/// it, its tile draws the review and holds the keyboard, and a second ask goes to that tile
/// rather than adding another. Comments added to the message land in the agent's draft, with
/// the agent's tile in front. The tile removed, the review view is let go.
#[gpui::test]
fn a_thread_s_review_opens_as_a_tile_of_its_own_and_goes_with_it(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let agent = opens(&view, cx, &studio, session, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let thread = state.meta.id;
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| {
        v.thread_table(key, &table, cx);
        v.focus_tile(agent, cx);
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    studio.drain();

    let ask = |cx: &mut VisualTestContext| {
        let face = view.read_with(cx, |v, _| v.thread_face(session).cloned()).expect("the face");
        face.update(cx, |_, cx| cx.emit(ThreadViewEvent::Review { thread }));
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    };
    ask(cx);
    let added: Vec<Item> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(item)) => Some(item),
            _ => None,
        })
        .collect();
    let [review] = added.as_slice() else { panic!("one review item: {added:?}") };
    assert_eq!(review.kind, ItemKind::Review { thread });

    let by = studio.me;
    let op = ItemOp::Add(review.clone());
    view.update_in(cx, |v, _w, cx| v.apply_sync(key, ItemSync::Delta { version: 2, by, op }, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let tile = TileRef { worker: key, item: review.id };
    assert_eq!(focused(&view, cx), Some(tile), "the review comes to the front");
    let keyboard = view.update_in(cx, |v, window, cx| {
        v.review_of(thread)
            .is_some_and(|r| Focusable::focus_handle(r.read(cx), cx).is_focused(window))
    });
    assert!(keyboard, "the review holds the keyboard");
    let title = view.read_with(cx, |v, _| v.tile_title(review));
    assert_eq!(title, tile::REVIEW);

    view.update_in(cx, |v, _w, cx| v.focus_tile(agent, cx));
    ask(cx);
    let again = studio.drain();
    assert!(!again.iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Add(_)))), "{again:?}");
    assert_eq!(focused(&view, cx), Some(tile), "a second ask goes to the open tile");

    let words = "In `src/lib.rs` line 11:\n```diff\n+    new();\n```\nWhy new?";
    let shown = view.read_with(cx, |v, _| v.review_of(thread).cloned()).expect("the review");
    shown.update(cx, |_, cx| {
        cx.emit(crate::review::ReviewEvent::AddToMessage { thread, text: words.to_owned(), id: 1 });
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let face = view.read_with(cx, |v, _| v.thread_face(session).cloned()).expect("the face");
    assert_eq!(
        face.read_with(cx, crate::conversation::thread::ThreadView::draft),
        words,
        "in the agent's draft"
    );
    assert_eq!(focused(&view, cx), Some(agent), "the agent's tile comes to the front");

    let op = ItemOp::Remove(review.id);
    view.update_in(cx, |v, _w, cx| v.apply_sync(key, ItemSync::Delta { version: 3, by, op }, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.review_of(thread).is_none()), "let go with its tile");
}

/// A thread is named the same everywhere: where its agent runs in a tile here, by that tile's
/// title, in the navigator, a thread tile's header and the author of a line in a review, not
/// by the title its worker's table gives it.
#[gpui::test]
fn a_thread_goes_by_its_agents_tile_everywhere(cx: &mut TestAppContext) {
    use slopty_proto::thread::TurnId;
    use slopty_proto::thread::wire::{AuthorRun, Authors};

    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let agent = opens(&view, cx, &studio, session, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    state.meta.title = "What the table says".to_owned();
    let thread = state.meta.id;
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table, cx));
    let by = studio.me;
    let named = ItemOp::Rename { id: agent.item, name: Some("Login fix".to_owned()) };
    view.update_in(cx, |v, _w, cx| {
        v.apply_sync(key, ItemSync::Delta { version: 2, by, op: named }, cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.thread_title(thread)), "Login fix");

    let review = view.update_in(cx, |v, window, cx| v.open_review(key, thread, window, cx));
    let authors = Authors {
        thread: Some(thread),
        path: "src/lib.rs".to_owned(),
        modified_ms: None,
        blob: Some("b".to_owned()),
        runs: vec![AuthorRun {
            start: 1,
            lines: 1,
            thread,
            turn: Some(TurnId(1)),
            commit: None,
            at_ms: WallMs::ZERO,
        }],
        absent: None,
    };
    review.update(cx, |r, cx| r.take_authors(0, authors, cx));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let named = review.read_with(cx, |r, _| r.writer(thread).map(|w| w.title.clone()));
    assert_eq!(named.as_deref(), Some("Login fix"));
}

/// "Review changes" on a folder opens its changes as a tile of their own on its machine, with
/// no thread: the tile asks the repository for what is not committed and shows it, the whole
/// branch on a click, with no keep or put back, which are for an agent, and no foot until a
/// comment waits. A second ask goes to that tile rather than adding another.
#[gpui::test]
fn a_folders_changes_open_as_a_tile_with_no_thread(cx: &mut TestAppContext) {
    use slopty_proto::RequestId;
    use slopty_proto::git::{GitDone, GitOp, GitOutcome};
    use slopty_proto::thread::Patch;
    use slopty_proto::thread::wire::{Against, FileDiff, Review, ReviewScope};

    use super::super::actions::ReviewChanges;
    use super::super::reviews::REVIEW_CHANGES;

    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let folder = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/atlas".into() }, 1);
    view.update_in(cx, |v, _w, cx| v.focus_tile(folder, cx));
    cx.run_until_parked();
    studio.drain();
    let offered = view.update(cx, |v, cx| v.palette_lines(cx));
    assert!(offered.iter().any(|l| l.label == REVIEW_CHANGES), "a folder's changes apply");

    cx.dispatch_action(ReviewChanges);
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let sent = studio.drain();
    let added: Vec<&Item> = sent
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(item)) => Some(item),
            _ => None,
        })
        .collect();
    let [item] = added.as_slice() else { panic!("one item: {added:?}") };
    assert_eq!(item.kind, ItemKind::Changes { path: "/w/atlas".into(), against: None });
    let changes = |sent: &[ClientMsg]| -> Vec<(RequestId, Against)> {
        sent.iter()
            .filter_map(|m| match m {
                ClientMsg::Git { request, repo, op: GitOp::Changes { against } }
                    if repo == "/w/atlas" =>
                {
                    Some((*request, against.clone()))
                }
                _ => None,
            })
            .collect()
    };
    let first = changes(&sent);
    let [(request, Against::Head)] = first.as_slice() else {
        panic!("what is not committed, asked: {first:?}")
    };
    let file = FileDiff {
        path: "src/lib.rs".to_owned(),
        from: Some("old".to_owned()),
        to: Some("new".to_owned()),
        binary: false,
        patch: Patch { hunks: Vec::new(), added: 1, removed: 0, clipped_lines: 0, full: None },
    };
    let review = Review {
        scope: ReviewScope::WorkingTree(Against::Head),
        from: None,
        to: None,
        files: vec![file],
        absent: None,
    };
    let done = GitOutcome::Done(GitDone::Changes(Box::new(review)));
    view.update_in(cx, |v, _w, cx| v.git_done(key, *request, done, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-head-0").is_some(), "the file shows");
    assert!(cx.debug_bounds("review-keep-file-0").is_none(), "nothing to keep for an agent");
    assert!(cx.debug_bounds("review-mark").is_none(), "and no foot");

    let branch = cx.debug_bounds("review-scope-WholeBranch").expect("the whole branch");
    cx.simulate_click(branch.center(), Modifiers::none());
    cx.run_until_parked();
    let then = changes(&studio.drain());
    assert!(matches!(then.as_slice(), [(_, Against::Base)]), "{then:?}");

    view.update_in(cx, |v, _w, cx| v.focus_tile(folder, cx));
    cx.dispatch_action(ReviewChanges);
    cx.run_until_parked();
    let again = studio.drain().into_iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Add(_))));
    assert!(!again, "the tile open already takes the focus");
}

/// Draw a few frames, as the quote waits for a composer to be made.
fn frames(cx: &mut VisualTestContext) {
    for _ in 0..6 {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
    }
    cx.run_until_parked();
}

/// The review of `thread`, asked for from `from`'s thread view and heard by the workspace.
fn review_from(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    from: &Entity<crate::conversation::thread::ThreadView>,
    thread: slopty_proto::thread::ThreadId,
) -> Entity<crate::review::ReviewView> {
    from.update(cx, |_, cx| cx.emit(ThreadViewEvent::Review { thread }));
    frames(cx);
    view.read_with(cx, |v, _| v.review_of(thread).cloned()).expect("the review")
}

/// `thread`'s row as `key`'s whole table, its link up.
fn table_of(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    key: WorkerKey,
    seq: u64,
    state: &slopty_proto::thread::ThreadState,
) {
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        v.thread_table(key, &table, cx);
    });
    frames(cx);
}

/// What the review's "Add to message" carries reaches a thread tile's composer, an agent with
/// no terminal (Codex over its app-server, pi over RPC, an ACP agent), and only then goes.
#[gpui::test]
fn added_comments_reach_a_thread_tile_and_go_only_then(cx: &mut TestAppContext) {
    use slopty_proto::thread::AgentId;
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let acp = format!("{}gemini", AgentId::ACP_PREFIX);
    for (agent, seq) in [AgentId::CODEX, AgentId::PI, acp.as_str()].into_iter().zip(1..) {
        let mut state = crate::conversation::thread::fixtures::thread("edit");
        state.meta.agent = AgentId::named(agent);
        let thread = state.meta.id;
        table_of(&view, cx, key, seq, &state);
        view.update_in(cx, |v, _w, cx| v.open_thread(key, thread, cx));
        frames(cx);
        let tile = view.read_with(cx, |v, _| v.tile_of_thread(thread)).expect("the thread's tile");
        let own = view.read_with(cx, |v, _| v.thread_item(tile.item).cloned()).expect("its view");
        let review = review_from(&view, cx, &own, thread);

        review.update(cx, |r, cx| r.add_note_to_message("Check the retry", cx));
        frames(cx);
        let draft = own.read_with(cx, crate::conversation::thread::ThreadView::draft);
        assert!(draft.contains("Check the retry"), "in {agent}'s draft: {draft:?}");
        assert_eq!(review.read_with(cx, |r, _| r.waiting()), 0, "taken by {agent}, so gone");
    }
}

/// Comments sent bring the thread's own tile to the front, an agent with no terminal as well
/// as one in a terminal: its answer shows there.
#[gpui::test]
fn sent_comments_bring_a_thread_tile_forward(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.agent = slopty_proto::thread::AgentId::named(slopty_proto::thread::AgentId::CODEX);
    let thread = state.meta.id;
    table_of(&view, cx, key, 1, &state);
    view.update_in(cx, |v, _w, cx| v.open_thread(key, thread, cx));
    frames(cx);
    let tile = view.read_with(cx, |v, _| v.tile_of_thread(thread)).expect("the thread's tile");
    let own = view.read_with(cx, |v, _| v.thread_item(tile.item).cloned()).expect("its view");
    let review = review_from(&view, cx, &own, thread);
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 9);
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    frames(cx);
    assert_eq!(focused(&view, cx), Some(shell), "away from the thread");

    review.update(cx, |_, cx| cx.emit(crate::review::ReviewEvent::CommentsSent { thread }));
    frames(cx);
    assert_eq!(focused(&view, cx), Some(tile), "the thread's tile, where its answer shows");
}

/// A thread whose machine is not connected here takes nothing: the comments stay in the review,
/// free to go again, and the person is told why.
#[gpui::test]
fn added_comments_for_a_machine_not_here_stay(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.agent = slopty_proto::thread::AgentId::named(slopty_proto::thread::AgentId::CODEX);
    let thread = state.meta.id;
    table_of(&view, cx, key, 1, &state);
    view.update_in(cx, |v, _w, cx| v.open_thread(key, thread, cx));
    frames(cx);
    let tile = view.read_with(cx, |v, _| v.tile_of_thread(thread)).expect("the thread's tile");
    let own = view.read_with(cx, |v, _| v.thread_item(tile.item).cloned()).expect("its view");
    let review = review_from(&view, cx, &own, thread);
    let (by, op) = (studio.me, ItemOp::Remove(tile.item));
    view.update_in(cx, |v, _w, cx| {
        v.apply_sync(key, ItemSync::Delta { version: 9, by, op }, cx);
        v.thread_table(
            key,
            &TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 2 }, rows: Vec::new() },
            cx,
        );
    });
    frames(cx);

    review.update(cx, |r, cx| r.add_note_to_message("Check the retry", cx));
    frames(cx);
    assert_eq!(review.read_with(cx, |r, _| r.waiting()), 1, "nothing lost");
    assert!(!review.read_with(cx, |r, _| r.comments_away()), "and free to go again");
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("That thread is on a machine not connected here"));
}

/// A Claude Code tile left on its TUI turns to its thread for the comments, which land in
/// the composer there.
#[gpui::test]
fn added_comments_turn_a_tui_tile_to_its_thread(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let session = SessionId::new();
    let agent = opens(&view, cx, &studio, session, studio.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.focus_tile(agent, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let thread = state.meta.id;
    table_of(&view, cx, key, 1, &state);
    let face = view.read_with(cx, |v, _| v.thread_face(session).cloned()).expect("the face");
    let review = review_from(&view, cx, &face, thread);
    view.update_in(cx, |v, _w, cx| v.show_face(session, false, cx));
    frames(cx);
    assert!(view.read_with(cx, |v, _| v.thread_face(session).is_none()), "on its TUI");

    review.update(cx, |r, cx| r.add_note_to_message("Check the retry", cx));
    frames(cx);
    let face = view.read_with(cx, |v, _| v.thread_face(session).cloned()).expect("the face again");
    let draft = face.read_with(cx, crate::conversation::thread::ThreadView::draft);
    assert!(draft.contains("Check the retry"), "in the thread's draft: {draft:?}");
    assert_eq!(review.read_with(cx, |r, _| r.waiting()), 0);
}

/// A thread shown in no tile gets one for the comments, which land in its composer.
#[gpui::test]
fn added_comments_open_a_tile_for_a_thread_shown_nowhere(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.agent = slopty_proto::thread::AgentId::named(slopty_proto::thread::AgentId::PI);
    let thread = state.meta.id;
    table_of(&view, cx, key, 1, &state);
    view.update_in(cx, |v, _w, cx| v.open_thread(key, thread, cx));
    frames(cx);
    let tile = view.read_with(cx, |v, _| v.tile_of_thread(thread)).expect("the thread's tile");
    let own = view.read_with(cx, |v, _| v.thread_item(tile.item).cloned()).expect("its view");
    let review = review_from(&view, cx, &own, thread);
    let (by, op) = (studio.me, ItemOp::Remove(tile.item));
    view.update_in(cx, |v, _w, cx| v.apply_sync(key, ItemSync::Delta { version: 9, by, op }, cx));
    frames(cx);
    assert!(view.read_with(cx, |v, _| v.tile_of_thread(thread)).is_none(), "shown nowhere");

    review.update(cx, |r, cx| r.add_note_to_message("Check the retry", cx));
    frames(cx);
    let tile = view.read_with(cx, |v, _| v.tile_of_thread(thread)).expect("a tile for it");
    let own = view.read_with(cx, |v, _| v.thread_item(tile.item).cloned()).expect("its view");
    let draft = own.read_with(cx, crate::conversation::thread::ThreadView::draft);
    assert!(draft.contains("Check the retry"), "in the thread's draft: {draft:?}");
    assert_eq!(review.read_with(cx, |r, _| r.waiting()), 0);
}

/// Comments no composer can take (the agent in the terminal has exited, so the tile has no
/// thread to show) stay in the review, and the person is told.
#[gpui::test]
fn added_comments_no_composer_takes_stay(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let session = SessionId::new();
    let agent = opens(&view, cx, &studio, session, studio.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.focus_tile(agent, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let thread = state.meta.id;
    table_of(&view, cx, key, 1, &state);
    let face = view.read_with(cx, |v, _| v.thread_face(session).cloned()).expect("the face");
    let review = review_from(&view, cx, &face, thread);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::None, ..blocked(session) }, cx);
    });
    frames(cx);

    review.update(cx, |r, cx| r.add_note_to_message("Check the retry", cx));
    frames(cx);
    assert!(review.read_with(cx, |r, _| r.comments_away()), "on their way while one may open");
    cx.executor().advance_clock(Duration::from_secs(5));
    frames(cx);
    assert_eq!(review.read_with(cx, |r, _| r.waiting()), 1, "nothing lost");
    assert!(!review.read_with(cx, |r, _| r.comments_away()), "and free to go again");
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("The agent's composer did not open"));
}

/// ⌘T from a thread's own tile, or from its review, opens the shell where its agent works, as
/// its worker's table says, not in the machine's home.
#[gpui::test]
fn a_shell_opened_from_a_thread_or_its_review_starts_where_the_agent_works(
    cx: &mut TestAppContext,
) {
    use slopty_proto::thread::AgentId;
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.agent = AgentId::named(AgentId::CODEX);
    state.meta.cwd = "/w/app".to_owned();
    let thread = state.meta.id;
    table_of(&view, cx, key, 1, &state);
    view.update_in(cx, |v, _w, cx| v.open_thread(key, thread, cx));
    frames(cx);
    let tile = view.read_with(cx, |v, _| v.tile_of_thread(thread)).expect("the thread's tile");
    let own = view.read_with(cx, |v, _| v.thread_item(tile.item).cloned()).expect("its view");
    let cwd_of_new_shell = |cx: &mut VisualTestContext, studio: &mut Fake| {
        studio.drain();
        view.update_in(cx, |v, w, cx| v.new_terminal(&NewTerminal, w, cx));
        cx.run_until_parked();
        studio.drain().into_iter().find_map(|m| match m {
            ClientMsg::OpenSession { spec, .. } => spec.cwd,
            _ => None,
        })
    };
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    assert_eq!(cwd_of_new_shell(cx, &mut studio).as_deref(), Some("/w/app"), "from the thread");

    let _review = review_from(&view, cx, &own, thread);
    let at = view.read_with(cx, |v, _| {
        v.layout().tiles().find(|t| {
            v.item(*t)
                .is_some_and(|i| matches!(i.kind, ItemKind::Review { thread: r } if r == thread))
        })
    });
    let at = at.expect("the review's tile");
    view.update_in(cx, |v, _w, cx| v.focus_tile(at, cx));
    assert_eq!(cwd_of_new_shell(cx, &mut studio).as_deref(), Some("/w/app"), "from its review");
}

/// The files added on `fake`'s machine since the last drain, by their paths.
fn files_added(fake: &mut Fake) -> Vec<String> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(Item { kind: ItemKind::File { path }, .. })) => Some(path),
            _ => None,
        })
        .collect()
}

/// A codex thread on `fake`'s machine in its own tile, and that tile.
fn thread_tile(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
) -> (slopty_proto::thread::ThreadId, TileRef) {
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.agent = slopty_proto::thread::AgentId::named(slopty_proto::thread::AgentId::CODEX);
    let thread = state.meta.id;
    let key = fake.key;
    table_of(view, cx, key, 1, &state);
    view.update_in(cx, |v, _w, cx| v.open_thread(key, thread, cx));
    frames(cx);
    let tile = view.read_with(cx, |v, _| v.tile_of_thread(thread)).expect("the thread's tile");
    (thread, tile)
}

/// A file's Open, from its menu in a review, opens the file in a tile on the review's machine:
/// from a thread's review and from a folder's alike.
#[gpui::test]
fn a_reviews_open_file_opens_it_on_its_machine(cx: &mut TestAppContext) {
    use crate::review::ReviewEvent;

    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (thread, tile) = thread_tile(&view, cx, &studio);
    let own = view.read_with(cx, |v, _| v.thread_item(tile.item).cloned()).expect("its view");
    let review = review_from(&view, cx, &own, thread);
    studio.drain();
    let path = "/w/src/lib.rs".to_owned();
    review.update(cx, |_, cx| cx.emit(ReviewEvent::OpenFile { path: path.clone() }));
    frames(cx);
    assert_eq!(files_added(&mut studio), [path], "a thread's review");

    let changes =
        arrives(&view, cx, &studio, ItemKind::Changes { path: "/w".into(), against: None }, 20);
    frames(cx);
    let folder = view.read_with(cx, |v, _| v.changes_view(changes.item).cloned());
    let folder = folder.expect("the folder's review");
    studio.drain();
    let path = "/w/README.md".to_owned();
    folder.update(cx, |_, cx| cx.emit(ReviewEvent::OpenFile { path: path.clone() }));
    frames(cx);
    assert_eq!(files_added(&mut studio), [path], "a folder's review");
}

/// A folder's review is heard as a thread's is: a press on who wrote a line opens that
/// thread, where before it went nowhere.
#[gpui::test]
fn a_folders_review_opens_the_thread_that_wrote_a_line(cx: &mut TestAppContext) {
    use crate::authorship::Opens;
    use crate::review::ReviewEvent;

    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (thread, tile) = thread_tile(&view, cx, &studio);
    let changes =
        arrives(&view, cx, &studio, ItemKind::Changes { path: "/w".into(), against: None }, 20);
    frames(cx);
    let folder = view.read_with(cx, |v, _| v.changes_view(changes.item).cloned());
    let folder = folder.expect("the folder's review");
    view.update_in(cx, |v, _w, cx| v.focus_tile(changes, cx));
    frames(cx);
    assert_eq!(focused(&view, cx), Some(changes), "on the folder's review");
    folder.update(cx, |_, cx| cx.emit(ReviewEvent::OpenThread(Opens { thread, turn: None })));
    frames(cx);
    assert_eq!(focused(&view, cx), Some(tile), "the thread that wrote it");
}

//! What a program in a worker's shell hands this client (a page, a file to edit), what this
//! client tells the worker back (what it takes, which shell is in front of the person), and an
//! agent's pull request on its tile.

use slopty_proto::agent::{AgentBranch, PullRequest, Review, Worktree};
use slopty_proto::file::{FileRead, WriteResult};
use slopty_proto::handoff::{
    EditFile, EditOutcome, HandoffCaps, HandoffEvent, HandoffReply, OfferReason, OpenUrl, Wary,
};

use super::*;

const COMMIT_MSG: &str = "/r/.git/COMMIT_EDITMSG";

fn handoff(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    key: WorkerKey,
    event: HandoffEvent,
) {
    view.update_in(cx, |v, _w, cx| v.handoff_event(key, event, Instant::now(), cx));
    cx.run_until_parked();
}

fn replies(sent: &[ClientMsg]) -> Vec<HandoffReply> {
    sent.iter()
        .filter_map(|m| match m {
            ClientMsg::Handoff(reply) => Some(*reply),
            _ => None,
        })
        .collect()
}

fn focus_reports(sent: &[ClientMsg]) -> Vec<(SessionId, bool)> {
    sent.iter()
        .filter_map(|m| match m {
            ClientMsg::Term { session, req: TermRequest::Focus { focused } } => {
                Some((*session, *focused))
            }
            _ => None,
        })
        .collect()
}

fn writes(sent: &[ClientMsg]) -> usize {
    sent.iter().filter(|m| matches!(m, ClientMsg::WriteFile { .. })).count()
}

/// `git commit` in `session` asks for its message, and waits.
fn commit_edit(id: u64, session: SessionId) -> HandoffEvent {
    HandoffEvent::Edit(EditFile {
        id,
        session: Some(session),
        path: COMMIT_MSG.to_owned(),
        line: Some(1),
        wait: true,
    })
}

/// The worker read the commit message file for its tile.
fn message_read(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, key: WorkerKey) {
    let read = FileRead::Text {
        text: "\n# Please enter the commit message".to_owned(),
        size: 34,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
        editorconfig: Vec::new(),
    };
    view.update_in(cx, |v, _w, cx| v.file_read(key, COMMIT_MSG, &read, cx));
    cx.run_until_parked();
}

fn saved(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, key: WorkerKey) {
    let saved = WriteResult::Saved { size: 40, modified_ms: WallMs::from_millis(2_000) };
    view.update_in(cx, |v, _w, cx| v.file_written(key, COMMIT_MSG, &saved, cx));
    cx.run_until_parked();
}

/// The file tile focused now, and the program waiting on it.
fn focused_file(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
) -> Option<(TileRef, Option<u64>)> {
    view.read_with(cx, |v, cx| {
        let tile = v.focused()?;
        let file = v.file(tile.item)?;
        Some((tile, file.read(cx).waiting()))
    })
}

/// Every link a worker comes up on starts with what this client takes, before the worker
/// would hand it anything: pages, and files (the workspace shows file tiles). A reconnect is
/// a new link, told again.
#[gpui::test]
fn every_link_starts_by_saying_which_handoffs_this_client_takes(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let key = WorkerKey::new(1);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    let link = |tx| WorkerLink {
        me: ClientId::new(),
        out: tx,
        open_screen: Arc::clone(&factory),
        remote: None,
    };
    let (tx, mut rx) = mpsc::channel(64);
    view.update_in(cx, |v, _w, cx| {
        v.add_worker(key, "studio".to_owned(), cx);
        v.connect_worker(key, link(tx), hello("studio", Vec::new()), cx);
    });
    let caps = ClientMsg::HandoffCaps(HandoffCaps { open: true, edit: true });
    assert_eq!(rx.try_recv().ok(), Some(caps.clone()), "the first word on the link");

    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("gone".to_owned()), cx);
    });
    let (tx, mut rx) = mpsc::channel(64);
    view.update_in(cx, |v, _w, cx| {
        v.connect_worker(key, link(tx), hello("studio", Vec::new()), cx);
    });
    assert_eq!(rx.try_recv().ok(), Some(caps), "said again on the new link");
}

/// `git commit` in a shell hands its message file over: it opens in a file tile right of that
/// shell, focused, and the worker hears it was taken. "Done" (⌘↩) saves the edit and answers
/// the program only once the worker has written it; the tile then stops waiting.
#[gpui::test]
fn an_edit_opens_beside_its_shell_and_done_answers_once_the_save_lands(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let [(asker, shell), (..), (_, last)] = three_shells(&view, cx, &studio);
    assert_eq!(focused(&view, cx), Some(last), "the person is elsewhere");
    studio.drain();

    handoff(&view, cx, studio.key, commit_edit(7, asker));
    let sent = studio.drain();
    assert_eq!(replies(&sent), [HandoffReply::Taken { id: 7 }], "{sent:?}");
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::ReadFile { path } if path == COMMIT_MSG)),
        "the tile reads the file: {sent:?}"
    );
    let (tile, waiting) = focused_file(&view, cx).expect("a focused file tile");
    assert_eq!(waiting, Some(7), "a program waits on it");
    assert_eq!(column_of(&view, cx, tile), column_of(&view, cx, shell).saturating_add(1));
    message_read(&view, cx, studio.key);
    assert!(cx.debug_bounds(selector("file-waiting", tile.item)).is_some(), "the bar says so");

    cx.simulate_input("Fix the build");
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    let sent = studio.drain();
    assert_eq!(writes(&sent), 1, "Done saves first: {sent:?}");
    assert!(replies(&sent).is_empty(), "and answers nothing before the disk has it: {sent:?}");

    saved(&view, cx, studio.key);
    let sent = studio.drain();
    assert_eq!(replies(&sent), [HandoffReply::Edited { id: 7, outcome: EditOutcome::Done }]);
    assert_eq!(focused_file(&view, cx).map(|(_, w)| w), Some(None), "it stops waiting");
    assert!(cx.debug_bounds(selector("file-waiting", tile.item)).is_none(), "the bar goes");
    let text = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| v.file(tile.item).map(|f| f.read(cx).text(cx)))
    };
    assert_eq!(text(cx).as_deref(), Some("Fix the build\n# Please enter the commit message"));
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    assert_eq!(
        text(cx).as_deref(),
        Some("Fix the build\n\n# Please enter the commit message"),
        "with nothing waiting, ⌘↩ is the editor's again"
    );
}

/// "Give up" answers the program at once without saving (`git commit` then aborts); a save
/// the worker refuses leaves the program waiting until the conflict is settled.
#[gpui::test]
fn give_up_answers_unsaved_and_a_refused_save_keeps_the_program_waiting(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let asker = SessionId::new();
    opens(&view, cx, &studio, asker, studio.me, 1);
    handoff(&view, cx, studio.key, commit_edit(3, asker));
    message_read(&view, cx, studio.key);
    let (tile, _) = focused_file(&view, cx).expect("a focused file tile");
    cx.simulate_input("wip");
    cx.simulate_keystrokes("cmd-enter");
    cx.run_until_parked();
    studio.drain();
    let refused = WriteResult::Conflict { modified_ms: WallMs::from_millis(3_000) };
    view.update_in(cx, |v, _w, cx| v.file_written(studio.key, COMMIT_MSG, &refused, cx));
    cx.run_until_parked();
    assert!(replies(&studio.drain()).is_empty(), "a refused save answers nothing");
    assert_eq!(focused_file(&view, cx).map(|(_, w)| w), Some(Some(3)), "still waiting");

    cx.debug_bounds(selector("file-give-up", tile.item)).expect("Give up is offered");
    view.update_in(cx, |v, _w, cx| {
        if let Some(file) = v.file(tile.item) {
            file.update(cx, FileView::give_up);
        }
    });
    cx.run_until_parked();
    let sent = studio.drain();
    assert_eq!(replies(&sent), [HandoffReply::Edited { id: 3, outcome: EditOutcome::Cancelled }]);
    assert_eq!(writes(&sent), 0, "given up unsaved");
}

/// Closing a tile a program waits on is "Done": the edit is saved and the program answered
/// once the worker wrote it, from the closed tile ⌘Z could still bring back.
#[gpui::test]
fn closing_a_waiting_tile_saves_and_answers_done(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let asker = SessionId::new();
    opens(&view, cx, &studio, asker, studio.me, 1);
    handoff(&view, cx, studio.key, commit_edit(4, asker));
    message_read(&view, cx, studio.key);
    cx.simulate_input("Ship it");
    cx.run_until_parked();
    studio.drain();
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    let sent = studio.drain();
    assert_eq!(writes(&sent), 1, "closing saves: {sent:?}");
    assert!(replies(&sent).is_empty(), "{sent:?}");
    saved(&view, cx, studio.key);
    assert_eq!(
        replies(&studio.drain()),
        [HandoffReply::Edited { id: 4, outcome: EditOutcome::Done }]
    );
}

/// The worker takes an edit back (its program went away): the tile stays, an ordinary file
/// tile, and answers nothing when closed. An edit asked again under the same number after a
/// reconnect only brings the tile forward.
#[gpui::test]
fn a_withdrawn_edit_leaves_a_plain_tile_and_an_edit_asked_again_is_the_same_tile(
    cx: &mut TestAppContext,
) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let asker = SessionId::new();
    let shell = opens(&view, cx, &studio, asker, studio.me, 1);
    handoff(&view, cx, studio.key, commit_edit(5, asker));
    let (tile, _) = focused_file(&view, cx).expect("a focused file tile");
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    handoff(&view, cx, studio.key, commit_edit(5, asker));
    assert_eq!(focused_file(&view, cx), Some((tile, Some(5))), "the same tile, forward");
    assert_eq!(view.read_with(cx, |v, _| v.len()), 2, "no second tile");

    handoff(&view, cx, studio.key, HandoffEvent::Withdrawn { id: 5 });
    assert_eq!(focused_file(&view, cx), Some((tile, None)), "a plain file tile now");
    studio.drain();
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert!(replies(&studio.drain()).is_empty(), "nothing waits on it");
}

/// A page the person just asked for opens in the browser, and the worker hears it was taken.
/// One held back (nobody typed, or an address built to deceive) opens nothing: the worker
/// hears it was offered, and a notice names the host; the worker taking it back takes the
/// notice down.
#[gpui::test]
fn a_page_opens_or_is_offered_by_its_host_until_withdrawn(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let open = |id, url: &str, offer| {
        HandoffEvent::Open(OpenUrl {
            id,
            session: None,
            url: url.to_owned(),
            asked_ms: WallMs::now(),
            offer,
        })
    };
    handoff(&view, cx, studio.key, open(1, "https://github.com/login/device", None));
    assert_eq!(cx.opened_url().as_deref(), Some("https://github.com/login/device"));
    assert_eq!(replies(&studio.drain()), [HandoffReply::Taken { id: 1 }]);

    handoff(
        &view,
        cx,
        studio.key,
        open(2, "https://example.test/a?b=c", Some(OfferReason::NotTyped)),
    );
    assert_eq!(
        replies(&studio.drain()),
        [HandoffReply::Offered { id: 2, why: OfferReason::NotTyped }]
    );
    assert_eq!(cx.opened_url().as_deref(), Some("https://github.com/login/device"), "not opened");
    assert_eq!(view.read_with(cx, |v, _| v.offered_hosts()), ["example.test"]);
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("studio wants to open example.test")
    );
    assert!(cx.debug_bounds("toast-open").is_some(), "with Open");

    handoff(&view, cx, studio.key, open(3, "https://github.com@evil.test/", None));
    assert_eq!(
        replies(&studio.drain()),
        [HandoffReply::Offered { id: 3, why: OfferReason::Wary(Wary::UserInfo) }],
        "this client checks the address whatever the worker let through"
    );
    assert_eq!(view.read_with(cx, |v, _| v.offered_hosts()), ["example.test", "evil.test"]);

    handoff(&view, cx, studio.key, HandoffEvent::Withdrawn { id: 2 });
    assert_eq!(view.read_with(cx, |v, _| v.offered_hosts()), ["evil.test"], "taken down");
    let open_button = cx.debug_bounds("toast-open").expect("Open");
    cx.simulate_click(open_button.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(cx.opened_url().as_deref(), Some("https://github.com@evil.test/"));
    assert_eq!(view.read_with(cx, |v, _| v.offered_hosts()), Vec::<String>::new());
    assert!(replies(&studio.drain()).is_empty(), "opening an offered page says nothing more");
}

/// The shell in front of the person is told to its worker: one `true` as its tile takes the
/// focus, one `false` as the focus leaves it or the window loses the keyboard, and nothing for
/// a change that leaves the same shell in front.
#[gpui::test]
fn the_shell_in_front_is_reported_once_each_way(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let a = SessionId::new();
    let tile_a = opens(&view, cx, &studio, a, studio.me, 1);
    assert_eq!(focus_reports(&studio.drain()), [(a, true)]);
    let b = SessionId::new();
    let tile_b = opens(&view, cx, &studio, b, studio.me, 2);
    assert_eq!(focus_reports(&studio.drain()), [(a, false), (b, true)]);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile_b, cx));
    cx.run_until_parked();
    assert!(focus_reports(&studio.drain()).is_empty(), "the same shell: nothing");

    view.update_in(cx, |v, _w, cx| v.set_app_active(false, cx));
    cx.run_until_parked();
    assert_eq!(focus_reports(&studio.drain()), [(b, false)], "the window resigned");
    view.update_in(cx, |v, _w, cx| v.set_app_active(true, cx));
    cx.run_until_parked();
    assert_eq!(focus_reports(&studio.drain()), [(b, true)]);

    view.update_in(cx, |v, _w, cx| v.focus_tile(tile_a, cx));
    cx.run_until_parked();
    assert_eq!(focus_reports(&studio.drain()), [(b, false), (a, true)]);
    let note = arrives(&view, cx, &studio, ItemKind::Note { text: String::new() }, 3);
    view.update_in(cx, |v, _w, cx| v.focus_tile(note, cx));
    cx.run_until_parked();
    assert_eq!(focus_reports(&studio.drain()), [(a, false)], "a note is no shell");
}

/// A new link starts focused on nothing, so the shell in front is told again on it.
#[gpui::test]
fn a_reconnect_reports_the_shell_in_front_again(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let a = SessionId::new();
    opens(&view, cx, &studio, a, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("gone".to_owned()), cx);
    });
    cx.run_until_parked();
    let (tx, mut rx) = mpsc::channel(256);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    view.update_in(cx, |v, _w, cx| {
        let link = WorkerLink { me: studio.me, out: tx, open_screen: factory, remote: None };
        v.connect_worker(key, link, hello("studio", vec![summary(a, None)]), cx);
    });
    cx.run_until_parked();
    let sent: Vec<ClientMsg> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert_eq!(focus_reports(&sent), [(a, true)], "{sent:?}");
}

/// An agent's pull request rides on its tile's header, toned by its review and named for a
/// screen reader, a click away from its page; its worktree beside it. Replaced by each word
/// from the worker, and gone with the agent.
#[gpui::test]
fn an_agents_pull_request_rides_on_its_header_while_the_agent_runs(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    let working = AgentEvent { status: AgentStatus::Working, ..blocked(session) };
    let branch = |review| AgentBranch {
        session,
        pr: Some(PullRequest {
            number: 1234,
            url: "https://github.com/o/r/pull/1234".to_owned(),
            review,
            merge_request: false,
        }),
        worktree: Some(Worktree {
            name: "fix-build".to_owned(),
            path: "/r/.claude/worktrees/fix-build".to_owned(),
            branch: Some("worktree-fix-build".to_owned()),
            original_cwd: "/r".to_owned(),
            original_branch: Some("main".to_owned()),
        }),
    };
    view.update_in(cx, |v, _w, cx| v.agent_branch(branch(Some(Review::Approved)), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("pr", tile.item)).is_none(), "no agent, no chip");

    view.update_in(cx, |v, _w, cx| v.agent_event(working.clone(), cx));
    cx.run_until_parked();
    let chip = cx.debug_bounds(selector("pr", tile.item)).expect("the chip");
    assert!(cx.debug_bounds(selector("worktree", tile.item)).is_some(), "the worktree");
    let said: Vec<String> = tree(cx).into_iter().filter_map(|n| n.label).collect();
    assert!(said.iter().any(|l| l == "Pull request 1234, approved"), "{said:?}");
    cx.simulate_click(chip.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(cx.opened_url().as_deref(), Some("https://github.com/o/r/pull/1234"));

    view.update_in(cx, |v, _w, cx| {
        v.agent_branch(AgentBranch { session, pr: None, worktree: None }, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("pr", tile.item)).is_none(), "the request went away");

    view.update_in(cx, |v, _w, cx| {
        v.agent_branch(branch(Some(Review::ChangesRequested)), cx);
        v.agent_event(AgentEvent { status: AgentStatus::None, ..working }, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("pr", tile.item)).is_none(), "gone with the agent");
}

/// A waiting tile closed while its save is refused is not left hanging: once it is closed for
/// good the program hears it was given up, and the edit is still kept on this device.
#[gpui::test]
fn a_waiting_tile_closed_with_its_save_refused_gives_up_and_keeps_the_edit(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = slopty_client::unsaved::Store::new(dir.path().join("unsaved"));
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_unsaved_store(store.clone(), cx));
    cx.run_until_parked();
    let mut studio = connect(&view, cx, 1, "studio");
    let asker = SessionId::new();
    opens(&view, cx, &studio, asker, studio.me, 1);
    handoff(&view, cx, studio.key, commit_edit(6, asker));
    message_read(&view, cx, studio.key);
    cx.simulate_input("wip");
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    let refused = WriteResult::Conflict { modified_ms: WallMs::from_millis(3_000) };
    view.update_in(cx, |v, _w, cx| v.file_written(studio.key, COMMIT_MSG, &refused, cx));
    cx.run_until_parked();
    studio.drain();
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    assert_eq!(
        replies(&studio.drain()),
        [HandoffReply::Edited { id: 6, outcome: EditOutcome::Cancelled }],
        "the program is let go"
    );
    let kept: Vec<String> = store.all().into_iter().map(|u| u.text).collect();
    assert_eq!(kept.len(), 1, "the unsaved edit is kept");
    assert!(kept[0].starts_with("wip"), "{kept:?}");
}

/// A waiting tile another client removes answers the program as given up.
#[gpui::test]
fn a_waiting_tile_removed_elsewhere_gives_up(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let asker = SessionId::new();
    opens(&view, cx, &studio, asker, studio.me, 1);
    handoff(&view, cx, studio.key, commit_edit(8, asker));
    let (tile, _) = focused_file(&view, cx).expect("a focused file tile");
    studio.drain();
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        let by = ClientId::new();
        v.apply_sync(key, ItemSync::Delta { version: 9, by, op: ItemOp::Remove(tile.item) }, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        replies(&studio.drain()),
        [HandoffReply::Edited { id: 8, outcome: EditOutcome::Cancelled }]
    );
}

/// An edit taken back before its tile was made leaves nothing behind: the tile, made later,
/// does not wait.
#[gpui::test]
fn an_edit_withdrawn_before_its_tile_is_made_leaves_no_wait(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let asker = SessionId::new();
    opens(&view, cx, &studio, asker, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.handoff_event(key, commit_edit(12, asker), Instant::now(), cx);
        v.handoff_event(key, HandoffEvent::Withdrawn { id: 12 }, Instant::now(), cx);
    });
    cx.run_until_parked();
    assert_eq!(focused_file(&view, cx).map(|(_, w)| w), Some(None), "a plain tile");
    let leaks = view.read_with(cx, |v, _| v.footprint());
    assert!(leaks.iter().all(|(name, n)| !name.starts_with("handoff.") || *n == 0), "{leaks:?}");
}

/// A shell whose program titled it at length still leaves the host in the offer's row.
#[gpui::test]
fn a_long_title_leaves_the_offers_host_in_view(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    opens(&view, cx, &studio, session, studio.me, 1);
    let title = "t".repeat(300);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        let mut named = summary(session, None);
        named.title.clone_from(&title);
        v.session_opened(key, named, cx);
    });
    cx.run_until_parked();
    let open = HandoffEvent::Open(OpenUrl {
        id: 1,
        session: Some(session),
        url: "https://example.test/".to_owned(),
        asked_ms: WallMs::now(),
        offer: Some(OfferReason::NotTyped),
    });
    handoff(&view, cx, key, open);
    let host = cx.debug_bounds("offer-host").expect("the host is drawn");
    let toast = cx.debug_bounds("offered").expect("the notice");
    assert!(host.size.width > px(0.0), "{host:?}");
    assert!(
        host.right() <= toast.right() && host.left() >= toast.left(),
        "the host inside its notice: {host:?} in {toast:?}"
    );
}

//! Closing, taking back and saving while a worker comes and goes: what the human did while it
//! was away lands when it is back, and nothing is left waiting on an answer that cannot come.

use slopty_core::WallMs;

use super::*;

/// The link to `fake`'s worker comes back on a new channel, with `sessions` running there;
/// the snapshot is not applied yet.
fn relink(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &mut Fake,
    sessions: Vec<SessionSummary>,
) {
    let (tx, rx) = mpsc::channel(256);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    let (key, me) = (fake.key, fake.me);
    view.update_in(cx, |v, _window, cx| {
        let link = WorkerLink { me, out: tx, open_screen: factory, remote: None };
        v.connect_worker(key, link, hello("studio", sessions), cx);
    });
    cx.run_until_parked();
    fake.rx = rx;
}

fn goes_away(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, fake: &Fake) {
    let key = fake.key;
    view.update_in(cx, |v, _window, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
}

/// A file tile whose text is `text`, focused, its editor holding the keyboard.
fn file_tile(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &mut Fake,
    path: &str,
) -> TileRef {
    let tile = arrives(view, cx, fake, ItemKind::File { path: path.to_owned() }, 1);
    let read = slopty_proto::file::FileRead::Text {
        text: "# Notes".to_owned(),
        size: 8,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
    };
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.file_read(key, path, &read, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    fake.drain();
    tile
}

fn file_state(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
    tile: TileRef,
) -> Option<(String, bool, bool)> {
    view.read_with(cx, |v, cx| {
        v.file(tile.item).map(|f| {
            let f = f.read(cx);
            (f.text(cx), f.dirty(), f.saving())
        })
    })
}

/// ⌘W on a file tile holding an edit, then ⌘Z: the tile comes back with the edit and still
/// dirty, not with the disk's text; it reads the file again to weigh the edit against it.
#[gpui::test]
fn a_closed_file_tile_comes_back_with_its_edit(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let path = "/w/notes.md";
    let tile = file_tile(&view, cx, &mut studio, path);
    cx.simulate_input("x");
    cx.run_until_parked();
    let edited = file_state(&view, cx, tile).expect("a file tile");
    assert!(edited.1 && edited.0.contains('x'), "{edited:?}");

    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert!(file_state(&view, cx, tile).is_none(), "off the strip");
    studio.drain();
    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    assert_eq!(file_state(&view, cx, tile), Some(edited), "the edit came back with it");
    let sent = studio.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::ReadFile { path: p } if p == path)),
        "{sent:?}"
    );
    assert!(cx.debug_bounds(selector("unsaved", tile.item)).is_some(), "and says so");
}

/// ⌘W on a shell while its worker is away: the tile goes at once, and when the worker is
/// back its snapshot (which still has the item) is not allowed to bring it back; the removal
/// and the session's close go to the worker in that order, so the shell does not run on there
/// unseen.
#[gpui::test]
fn a_shell_closed_while_its_worker_is_away_is_closed_when_it_is_back(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let [(s1, first), (s2, second), (s3, third)] = three_shells(&view, cx, &studio);
    view.update_in(cx, |v, _w, cx| v.focus_tile(second, cx));
    cx.run_until_parked();
    goes_away(&view, cx, &studio);
    studio.drain();
    let held = cx.update(|window, cx| view.read(cx).focus.contains_focused(window, cx));
    assert!(held, "the workspace keeps the keyboard when the focused shell's view goes");
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.layout().contains(second)), "off the strip");
    cx.executor().advance_clock(UNDO_CLOSE);
    cx.run_until_parked();
    assert!(studio.drain().is_empty(), "nothing can reach the worker yet");

    let sessions = [s1, s2, s3].map(|s| summary(s, None)).to_vec();
    relink(&view, cx, &mut studio, sessions);
    let items: Vec<Item> = view
        .read_with(cx, |v, _| [first, third].iter().filter_map(|t| v.item(*t).cloned()).collect());
    let gone = Item {
        id: second.item,
        kind: ItemKind::Terminal { session: s2 },
        sleeping: false,
        name: None,
    };
    let snapshot = ItemSync::Snapshot { version: 9, items: [items, vec![gone]].concat() };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.apply_sync(key, snapshot, cx));
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.layout().contains(second)), "the snapshot keeps it off");
    let sent: Vec<ClientMsg> = studio
        .drain()
        .into_iter()
        .filter(|m| {
            matches!(m, ClientMsg::Items(_) | ClientMsg::Term { req: TermRequest::Close, .. })
        })
        .collect();
    assert!(
        matches!(
            sent.as_slice(),
            [
                ClientMsg::Items(ItemOp::Remove(item)),
                ClientMsg::Term { session, req: TermRequest::Close },
            ] if *item == second.item && *session == s2
        ),
        "{sent:?}"
    );
}

/// A save whose link drops before the worker answers stops waiting and says so; ⌘S while the
/// worker is away says at once that it cannot reach it; the link coming back reads the file
/// again. At no point is the tile left saving, which would hold ⌘S, Overwrite and Reload.
#[gpui::test]
fn a_save_is_never_left_waiting_on_a_worker_that_went_away(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let path = "/w/notes.md";
    let tile = file_tile(&view, cx, &mut studio, path);
    cx.simulate_input("x");
    cx.simulate_keystrokes("cmd-s");
    cx.run_until_parked();
    assert!(file_state(&view, cx, tile).is_some_and(|(_, _, saving)| saving), "sent");

    goes_away(&view, cx, &studio);
    let trouble = |view: &Entity<WorkspaceView>, cx: &VisualTestContext| {
        view.read_with(cx, |v, cx| v.file(tile.item).and_then(|f| f.read(cx).trouble().cloned()))
    };
    assert_eq!(file_state(&view, cx, tile).map(|s| (s.1, s.2)), Some((true, false)));
    assert!(matches!(trouble(&view, cx), Some(crate::file::Trouble::Failed(_))));

    cx.simulate_keystrokes("cmd-s");
    cx.run_until_parked();
    assert_eq!(file_state(&view, cx, tile).map(|s| s.2), Some(false), "not left saving");
    assert!(
        matches!(trouble(&view, cx), Some(crate::file::Trouble::Failed(e)) if e.contains("studio")),
        "{:?}",
        trouble(&view, cx)
    );

    relink(&view, cx, &mut studio, Vec::new());
    let sent = studio.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::ReadFile { path: p } if p == path)),
        "{sent:?}"
    );
}

/// The bell counts a finished command only while its session and its worker are there: a
/// session that ends, or a worker forgotten, takes its badge with it.
#[gpui::test]
fn a_finished_badge_goes_with_its_session_and_its_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(s1, _), (s2, _), _] = three_shells(&view, cx, &studio);
    let done =
        || Finished { command: "make".into(), exit: Some(0), elapsed: Duration::from_secs(9) };
    view.update_in(cx, |v, _w, cx| {
        v.command_finished(s1, done(), cx);
        v.command_finished(s2, done(), cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.inbox_count()), 2);
    view.update_in(cx, |v, _w, cx| v.session_closed(s1, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.inbox_count()), 1, "the ended session's went");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.remove_worker(key, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.inbox_count()), 0, "the forgotten worker's went");
}

/// Two tiles closed, the second taken back: the first one's offer stays up.
#[gpui::test]
fn taking_one_tile_back_leaves_the_other_offer(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(_, first), _, (_, third)] = three_shells(&view, cx, &studio);
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| v.focus_tile(third, cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.toast_texts()).len(), 2);
    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.layout().contains(third)), "the latest came back");
    let left = view.read_with(cx, |v, _| v.toast_texts());
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].starts_with("Closed"), "{left:?}");
}

/// A link in doubt after a resume sets its worker's tiles back in the frame it starts, over
/// what they showed, with no pill; the frame the probe answers brings them back. A relink made
/// before the old link is let go swaps the old views for the new link's in one step, so no frame
/// shows the tile empty or saying the worker is away. Every frame is the one drawn from scratch.
#[gpui::test]
fn a_link_in_doubt_sets_its_tiles_back_until_it_is_live(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let [(shell, tile), ..] = three_shells(&view, cx, &studio);
    let fresh = |cx: &mut VisualTestContext, step: &str| {
        cx.run_until_parked();
        let stale = cx.update(|window, cx| crate::retained::stale(window, cx, 12));
        assert!(stale.is_none(), "{step}: the window shows a stale frame. {stale:?}");
    };
    let in_doubt: &'static str =
        Box::leak(format!("in-doubt-{}", tile.item.as_uuid()).into_boxed_str());
    let pill: &'static str = Box::leak(format!("state-{}", tile.item.as_uuid()).into_boxed_str());
    let key = studio.key;
    let status = |view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, status| {
        view.update_in(cx, |v, _w, cx| v.set_worker_status(key, status, cx));
        cx.run_until_parked();
    };
    fresh(cx, "the shells");
    assert!(cx.debug_bounds(in_doubt).is_none());

    status(&view, cx, WorkerStatus::Checking);
    assert!(cx.debug_bounds(in_doubt).is_some(), "set back in the frame the doubt starts");
    assert!(cx.debug_bounds(pill).is_none(), "with no word over it");
    assert!(view.read_with(cx, |v, _| v.terminal(shell).is_some()), "showing what it showed");
    fresh(cx, "checking");
    status(&view, cx, WorkerStatus::Connected);
    assert!(cx.debug_bounds(in_doubt).is_none(), "back the frame the probe answers");
    fresh(cx, "answered");

    status(&view, cx, WorkerStatus::Relinking);
    assert!(cx.debug_bounds(in_doubt).is_some() && cx.debug_bounds(pill).is_none());
    // The new link lands: the old views go and the new link's come in the same update.
    let (tx, rx) = mpsc::channel(256);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    let me = studio.me;
    let sessions = vec![summary(shell, None)];
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Relinking, cx);
        let link = WorkerLink { me, out: tx, open_screen: factory, remote: None };
        v.connect_worker(key, link, hello("studio", sessions), cx);
    });
    studio.rx = rx;
    cx.run_until_parked();
    assert!(cx.debug_bounds(in_doubt).is_none(), "live again");
    assert!(cx.debug_bounds(pill).is_none(), "and no frame said the worker was away");
    assert_eq!(view.read_with(cx, |v, _| v.links(key)), 2, "on its second link");
    fresh(cx, "relinked");
}

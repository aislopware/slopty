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
        editorconfig: Vec::new(),
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
    let path = "/w/notes.txt";
    let tile = file_tile(&view, cx, &mut studio, path);
    cx.simulate_input("x");
    cx.run_until_parked();
    let edited = file_state(&view, cx, tile).expect("a file tile");
    assert!(edited.1 && edited.0.contains('x'), "{edited:?}");

    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert!(file_state(&view, cx, tile).is_none(), "out of the layout");
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
    let shell = view.read_with(cx, |v, _| v.terminal(s2).cloned()).expect("kept through the drop");
    let held = cx.update(|window, cx| shell.read(cx).focus_handle(cx).is_focused(window));
    assert!(held, "the shell keeps its view, and the keyboard with it");
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.layout().contains(second)), "out of the layout");
    cx.executor().advance_clock(UNDO_CLOSE);
    cx.executor().advance_clock(IDLE_SHELL_KEPT);
    cx.run_until_parked();
    assert!(studio.drain().is_empty(), "nothing can reach the worker yet");

    let sessions = [s1, s2, s3].map(|s| summary(s, None)).to_vec();
    relink(&view, cx, &mut studio, sessions);
    let items: Vec<Item> = view
        .read_with(cx, |v, _| [first, third].iter().filter_map(|t| v.item(*t).cloned()).collect());
    let gone = Item {
        id: second.item,
        kind: ItemKind::Terminal { session: s2 },
        name: None,
        facts: BTreeMap::new(),
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
    let path = "/w/notes.txt";
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

/// A finished command's badge stays only while its session and its worker are there: a
/// session that ends, or a worker forgotten, takes it with it.
#[gpui::test]
fn a_finished_badge_goes_with_its_session_and_its_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(s1, _), (s2, _), _] = three_shells(&view, cx, &studio);
    let done =
        || Finished { command: "make".into(), exit: Some(0), elapsed: Duration::from_secs(40) };
    view.update_in(cx, |v, _w, cx| {
        v.command_finished(s1, done(), cx);
        v.command_finished(s2, done(), cx);
    });
    cx.run_until_parked();
    let badged = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| [s1, s2].map(|s| v.finished(s).is_some()))
    };
    assert_eq!(badged(cx), [true, true]);
    view.update_in(cx, |v, _w, cx| v.session_closed(s1, cx));
    cx.run_until_parked();
    assert_eq!(badged(cx), [false, true], "the ended session's went");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.remove_worker(key, cx));
    cx.run_until_parked();
    assert_eq!(badged(cx), [false, false], "the forgotten worker's went");
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
    let (view, cx) = still_workspace(cx);
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

/// The rows `session`'s view shows, trimmed.
fn rows(view: &Entity<WorkspaceView>, cx: &VisualTestContext, session: SessionId) -> Vec<String> {
    view.read_with(cx, |v, cx| {
        let shell = v.terminal(session).expect("a view").read(cx);
        shell.state().screen().lines().iter().map(|l| l.text().trim_end().to_owned()).collect()
    })
}

/// Every way a link drops (a silence, the worker restarting, the server saying it went away)
/// keeps a shell's view with its last rows, set back under a pill saying so. The next link
/// takes the same view up: it attaches at the size it is laid out at, with this client's
/// colours, and the restarted worker's first frame replaces the rows though its numbers start
/// over. A session the worker no longer runs lets go of its view with its tile.
#[gpui::test]
fn a_dropped_link_keeps_each_shell_and_the_next_one_takes_it_up_in_place(cx: &mut TestAppContext) {
    let statuses = [
        WorkerStatus::Reconnecting("disconnected: worker silent".into()),
        WorkerStatus::Reconnecting("disconnected: closed by peer".into()),
        WorkerStatus::Unreachable,
        WorkerStatus::Gone,
    ];
    for status in statuses {
        let (view, cx) = workspace(cx);
        let mut studio = connect(&view, cx, 1, "studio");
        let [(s1, first), (s2, second), _] = three_shells(&view, cx, &studio);
        view.update_in(cx, |v, _w, cx| {
            v.term_event(s1, frame(&["$ make", "built"]), cx);
            v.focus_tile(first, cx);
        });
        cx.run_until_parked();
        let shell = view.read_with(cx, |v, _| v.terminal(s1).cloned()).expect("attached");
        let key = studio.key;
        view.update_in(cx, |v, _w, cx| v.disconnect_worker(key, status.clone(), cx));
        cx.run_until_parked();
        let kept = view.read_with(cx, |v, _| v.terminal(s1).cloned()).expect("kept");
        assert_eq!(kept.entity_id(), shell.entity_id(), "{status:?}: the same view");
        assert_eq!(rows(&view, cx, s1)[..2], ["$ make", "built"], "{status:?}: its last rows");
        assert!(cx.debug_bounds(selector("in-doubt", first.item)).is_some(), "{status:?}");
        assert!(cx.debug_bounds(selector("state", first.item)).is_some(), "{status:?}: a pill");
        // Another worker's sync reconciles every view: the away worker's stay.
        let laptop = connect(&view, cx, 2, "laptop");
        opens(&view, cx, &laptop, SessionId::new(), laptop.me, 1);
        assert!(view.read_with(cx, |v, _| v.terminal(s2).is_some()), "{status:?}");

        relink(&view, cx, &mut studio, vec![summary(s1, None)]);
        let sent = studio.drain();
        let attached = sent.iter().position(|m| {
            matches!(m, ClientMsg::Term { session, req: TermRequest::Attach { size } }
                if *session == s1 && size.cols > 0)
        });
        let colours = sent.iter().position(|m| {
            matches!(m, ClientMsg::Term { session, req: TermRequest::Colors(_) } if *session == s1)
        });
        assert!(attached.zip(colours).is_some_and(|(a, c)| a < c), "{status:?}: {sent:?}");
        let relinked = view.read_with(cx, |v, _| v.terminal(s1).cloned()).expect("still there");
        assert_eq!(relinked.entity_id(), shell.entity_id(), "{status:?}: taken up in place");
        assert_eq!(rows(&view, cx, s1)[..2], ["$ make", "built"], "{status:?}: until a frame");
        assert!(cx.debug_bounds(selector("in-doubt", first.item)).is_none(), "{status:?}");
        assert!(cx.debug_bounds(selector("state", first.item)).is_none(), "{status:?}");

        let restarted = match frame(&["$ make", "built", "$ "]) {
            TermEvent::Frame(f) => TermEvent::Frame(Frame { seq: 0, ..f }),
            other => other,
        };
        view.update_in(cx, |v, _w, cx| v.term_event(s1, restarted, cx));
        cx.run_until_parked();
        assert_eq!(rows(&view, cx, s1)[..3], ["$ make", "built", "$"], "{status:?}");

        let items: Vec<Item> =
            view.read_with(cx, |v, _| v.item(first).cloned().into_iter().collect());
        view.update_in(cx, |v, _w, cx| {
            v.apply_sync(key, ItemSync::Snapshot { version: 9, items }, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert!(v.terminal(s2).is_none(), "{status:?}: the ended session's view went");
            assert!(!v.layout().contains(second), "{status:?}: with its tile");
        });
    }
}

/// A remote window whose link drops keeps its last picture, set back. The next link opens its
/// stream again behind that picture: the old view stays on the tile, and hears nothing of the
/// new link's streams (whose numbers start over), until the new stream has a picture of its
/// own, which takes its place in one step.
#[cfg(target_os = "macos")]
#[gpui::test]
fn a_dropped_window_keeps_its_picture_until_the_new_stream_has_one(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let window = slopty_core::WindowId(7);
    let tile = arrives(&view, cx, &studio, ItemKind::Window { window }, 1);
    let opened = || ScreenEvent::Opened {
        stream: StreamId(1),
        target: CaptureTarget::Window(window),
        codec: slopty_proto::screen::VideoCodec::Hevc,
        width: 1280,
        height: 800,
        scale: 2.0,
        stripes: Vec::new(),
    };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.screen_event(key, opened(), cx));
    cx.run_until_parked();
    let picture = || {
        core_video::pixel_buffer::CVPixelBuffer::new(
            core_video::pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            64,
            40,
            None,
        )
        .expect("pixel buffer")
    };
    let old = view.read_with(cx, |v, _| v.screen(tile.item).cloned()).expect("streaming");
    old.update(cx, |v, cx| v.show_picture(picture(), cx));
    cx.run_until_parked();

    goes_away(&view, cx, &studio);
    let kept = view.read_with(cx, |v, _| v.screen(tile.item).cloned()).expect("kept");
    assert_eq!(kept.entity_id(), old.entity_id(), "the last picture stays");
    assert!(cx.debug_bounds(selector("in-doubt", tile.item)).is_some(), "set back");

    relink(&view, cx, &mut studio, Vec::new());
    let items: Vec<Item> = view.read_with(cx, |v, _| v.item(tile).cloned().into_iter().collect());
    view.update_in(cx, |v, _w, cx| {
        v.apply_sync(key, ItemSync::Snapshot { version: 9, items }, cx);
    });
    cx.run_until_parked();
    let sent = studio.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::Screen(ScreenRequest::Open { .. }))),
        "the new link opens the stream again: {sent:?}"
    );
    view.update_in(cx, |v, _w, cx| v.screen_event(key, opened(), cx));
    cx.run_until_parked();
    let fresh = view
        .read_with(cx, |v, _| v.workers.get(&key)?.fresh_screens.get(&tile.item).cloned())
        .expect("opened behind the old picture");
    let shown = view.read_with(cx, |v, _| v.screen(tile.item).map(Entity::entity_id));
    assert_eq!(shown, Some(old.entity_id()), "the old picture until the new one shows");
    assert!(cx.debug_bounds(selector("in-doubt", tile.item)).is_some(), "still set back");
    let geometry =
        ScreenEvent::Geometry { stream: StreamId(1), width: 640, height: 400, stripes: Vec::new() };
    view.update_in(cx, |v, _w, cx| v.screen_event(key, geometry, cx));
    cx.run_until_parked();
    assert_eq!(fresh.read_with(cx, |v, _| v.native()), (640.0, 400.0), "the new link's word");
    assert_ne!(old.read_with(cx, |v, _| v.native()), (640.0, 400.0), "never the old view's");

    fresh.update(cx, |v, cx| v.show_picture(picture(), cx));
    cx.run_until_parked();
    let shown = view.read_with(cx, |v, _| v.screen(tile.item).map(Entity::entity_id));
    assert_eq!(shown, Some(fresh.entity_id()), "the new stream took the tile");
    assert!(cx.debug_bounds(selector("in-doubt", tile.item)).is_none(), "live again");
    let waiting = view.read_with(cx, |v, _| v.workers.get(&key).map(|w| w.fresh_screens.len()));
    assert_eq!(waiting, Some(0));
}

/// ⌘⇧T and ⌘O on a worker out of reach make nothing and say so, rather than dropping the ask
/// unseen: the only worker, or the one "+" chose with another up. ⌘O asks for no list, so no
/// picker turns up once the worker is back. A machine "+" was pointed at is let go when the
/// menu closes with nothing chosen.
#[gpui::test]
fn opens_on_a_worker_out_of_reach_are_said_and_not_kept(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let _tile = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    goes_away(&view, cx, &fake);
    fake.drain();

    cx.simulate_keystrokes("cmd-shift-t");
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("The terminal did not open: studio is lost; reconnecting…")
    );
    let mut laptop = connect(&view, cx, 2, "laptop");
    let key = fake.key;
    view.update_in(cx, |v, _w, _cx| v.new_on = Some(key));
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("The window picker did not open: studio is lost; reconnecting…"),
        "the machine \"+\" chose, not the one that is up"
    );
    assert!(laptop.drain().is_empty(), "nothing went to the laptop");
    let (picker, wanted) =
        view.read_with(cx, |v, _| (v.picker.is_some(), v.workers[&key].picker_wanted));
    assert!(!picker && !wanted, "no picker now, and none asked for later");
    relink(&view, cx, &mut fake, Vec::new());
    let sent = fake.drain();
    assert!(!sent.iter().any(|m| matches!(m, ClientMsg::OpenSession { .. })), "{sent:?}");

    view.update_in(cx, |v, window, cx| {
        v.new_on = Some(key);
        v.menu = Some(titlebar::MenuKind::New);
        v.dismiss_menu(window, cx);
    });
    assert_eq!(view.read_with(cx, |v, _| v.new_on), None, "the choice went with the menu");
}

/// The away pill says why the link dropped and offers what can be done from here: a dial now
/// while the app can reach the host, a wake while the machine sleeps or is gone, and the
/// tailnet grant to copy when the policy turns this device away. A tile whose link is up
/// offers none of them.
#[gpui::test]
fn the_away_pill_says_why_and_offers_the_way_back(cx: &mut TestAppContext) {
    use std::cell::Cell;
    use std::rc::Rc;

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let key = studio.key;
    let (dialled, woke) = (Rc::new(Cell::new(0_u32)), Rc::new(Cell::new(0_u32)));
    let count = |n: &Rc<Cell<u32>>| -> MenuRun {
        let n = Rc::clone(n);
        Rc::new(move |_w, _cx| n.set(n.get().saturating_add(1)))
    };
    let actions = HostActions {
        connect: Some(count(&dialled)),
        wake: Some(count(&woke)),
        ..HostActions::default()
    };
    view.update_in(cx, |v, _w, cx| {
        v.set_host_actions(std::iter::once((key, actions)).collect(), None, cx);
        v.set_tailnet_grant(Some("grant-json".to_owned()));
    });
    cx.run_until_parked();
    let drawn = |cx: &mut VisualTestContext, part: &str| {
        cx.debug_bounds(selector(part, shell.item)).is_some()
    };
    for part in ["away-why", "retry-worker", "wake-worker", "copy-grant"] {
        assert!(!drawn(cx, part), "{part}: the link is up");
    }
    let press = |cx: &mut VisualTestContext, part: &str| {
        let at = cx.debug_bounds(selector(part, shell.item)).expect("drawn");
        cx.simulate_click(at.center(), Modifiers::default());
        cx.run_until_parked();
    };

    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("worker silent\nfor 9 s".into()), cx);
    });
    cx.run_until_parked();
    assert!(drawn(cx, "away-why"), "the reason, its first line");
    assert!(!drawn(cx, "wake-worker") && !drawn(cx, "copy-grant"), "a dropped link only");
    press(cx, "retry-worker");
    assert_eq!(dialled.get(), 1, "retry dials now");

    view.update_in(cx, |v, _w, cx| v.disconnect_worker(key, WorkerStatus::Unreachable, cx));
    cx.run_until_parked();
    press(cx, "wake-worker");
    assert_eq!(woke.get(), 1, "a machine that cannot be reached is woken");
    assert!(!drawn(cx, "copy-grant"));

    view.update_in(cx, |v, _w, cx| v.disconnect_worker(key, WorkerStatus::NotGranted, cx));
    cx.run_until_parked();
    assert!(!drawn(cx, "wake-worker"), "waking does not change the policy");
    press(cx, "copy-grant");
    let copied = cx.update(|_w, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(copied.as_deref(), Some("grant-json"));
    let notice = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(notice.as_deref(), Some(tile::GRANT_COPIED));

    view.update_in(cx, |v, _w, cx| {
        v.set_tailnet_grant(None);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(!drawn(cx, "copy-grant"), "no grant to copy, no button");
}

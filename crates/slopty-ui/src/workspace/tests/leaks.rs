//! Nothing a tile, an overlay or a worker made outlives it. Each case opens something and
//! closes it, lets every wait that holds it run out, then asks GPUI's leak detector whether any
//! entity made since is still held, GPUI whether it holds more callbacks (observers,
//! subscriptions, release and window listeners) than before, and the workspace whether any of
//! its maps and lists kept an entry. `LEAK_BACKTRACE=1` names where each surviving handle was
//! made.

use std::cell::Cell;

use slopty_proto::thread::Cursor;
use slopty_proto::thread::wire::{ThreadFrame, ThreadRequest};

use super::*;

/// Longer than every wait a closed thing is held for: the undo of a close, its toast, the
/// palette's way out.
pub(super) const SETTLE: Duration = Duration::from_secs(10);

/// Every timer run out, then two frames drawn, so element state kept by the frames that showed
/// the closed thing is gone with them.
pub(super) fn settle(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
    cx.executor().advance_clock(SETTLE);
    cx.run_until_parked();
    for _ in 0..2 {
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
    }
}

/// Runs `cycle` (an open, then its close) once to make what is made once per app, then again
/// between a snapshot and the check: no entity the second run made is still held, and every map
/// and list of the workspace is as long as before either run.
fn closes_clean(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    cycle: impl FnMut(&mut VisualTestContext),
) {
    closes_clean_times(view, cx, 1, cycle);
}

/// [`closes_clean`] with `times` runs between the snapshot and the check, each settled, so a
/// leak too small to see once shows as growth. The callbacks GPUI holds are counted after the
/// first run too: a view's are held until its window draws twice after it goes, which each
/// settling does.
pub(super) fn closes_clean_times(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    times: usize,
    mut cycle: impl FnMut(&mut VisualTestContext),
) {
    settle(view, cx);
    let before = view.read_with(cx, |v, _| v.footprint());
    cycle(cx);
    settle(view, cx);
    let snapshot = cx.update(|_, cx| cx.leak_detector_snapshot());
    let subscribed = cx.update(|_, cx| cx.subscription_counts());
    for _ in 0..times {
        cycle(cx);
        settle(view, cx);
    }
    cx.update(|_, cx| cx.assert_no_new_leaks(&snapshot));
    let now = cx.update(|_, cx| cx.subscription_counts());
    assert_eq!(
        now,
        subscribed,
        "a callback outlived what it watched: {} against {}",
        now.total(),
        subscribed.total()
    );
    assert_eq!(view.read_with(cx, |v, _| v.footprint()), before, "a collection kept an entry");
}

/// The next registry version, for the items a cycle brings.
pub(super) fn next(version: &Cell<u64>) -> u64 {
    version.set(version.get().saturating_add(1));
    version.get()
}

/// `tile` focused, then ⌘W.
pub(super) fn close(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tile: TileRef) {
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
}

/// A workspace with one worker and one shell on it, in the folder the cases' files and folders
/// are in so they arrive beside it, which stays while the cases come and go.
pub(super) fn studio(
    cx: &mut TestAppContext,
) -> (Entity<WorkspaceView>, &mut VisualTestContext, Fake) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    opens_in(&view, cx, &fake, SessionId::new(), fake.me, 1, Some("/w"));
    (view, cx, fake)
}

/// A shell with a screen of output, closed: its view goes after the undo window and the
/// worker's word that the session ended.
#[gpui::test]
fn a_closed_terminal_leaves_nothing(cx: &mut TestAppContext) {
    let (view, cx, fake) = studio(cx);
    let version = Cell::new(1);
    closes_clean(&view, cx, |cx| {
        let session = SessionId::new();
        let tile = opens(&view, cx, &fake, session, fake.me, next(&version));
        view.update_in(cx, |v, _w, cx| v.term_event(session, frame(&["~ % ls", "a b"]), cx));
        close(&view, cx, tile);
        cx.executor().advance_clock(UNDO_CLOSE);
        cx.run_until_parked();
        view.update_in(cx, |v, _w, cx| v.session_closed(session, cx));
    });
}

/// A remote window streaming, closed.
#[gpui::test]
fn a_closed_stream_leaves_nothing(cx: &mut TestAppContext) {
    let (view, cx, fake) = studio(cx);
    let version = Cell::new(1);
    let key = fake.key;
    closes_clean(&view, cx, |cx| {
        let n = next(&version);
        let id = u32::try_from(n).unwrap();
        let window = slopty_core::WindowId(id);
        let tile = arrives(&view, cx, &fake, ItemKind::Window { window }, n);
        view.update_in(cx, |v, _w, cx| {
            let opened = ScreenEvent::Opened {
                stream: StreamId(id),
                target: CaptureTarget::Window(window),
                codec: slopty_proto::screen::VideoCodec::Hevc,
                width: 1280,
                height: 800,
                scale: 2.0,
                stripes: Vec::new(),
            };
            v.screen_event(key, opened, cx);
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.screen(tile.item).is_some()), "streaming");
        close(&view, cx, tile);
    });
}

/// A file tile with an edit not saved, closed: the editor waits out the undo window with the
/// closed tile, then goes, edit and all.
#[gpui::test]
fn a_closed_file_with_an_edit_leaves_nothing(cx: &mut TestAppContext) {
    let (view, cx, fake) = studio(cx);
    let version = Cell::new(1);
    let key = fake.key;
    closes_clean(&view, cx, |cx| {
        let path = format!("/w/notes-{}.txt", version.get());
        let tile = arrives(&view, cx, &fake, ItemKind::File { path: path.clone() }, next(&version));
        let text = slopty_proto::file::FileRead::Text {
            text: "# Notes".to_owned(),
            size: 8,
            modified_ms: WallMs::from_millis(1_000),
            final_newline: true,
            editorconfig: Vec::new(),
        };
        view.update_in(cx, |v, _w, cx| {
            v.file_read(key, &path, &text, cx);
            v.focus_tile(tile, cx);
        });
        cx.run_until_parked();
        cx.simulate_input("x");
        cx.run_until_parked();
        close(&view, cx, tile);
    });
}

/// A page tile, a note and a folder, each closed.
#[gpui::test]
fn a_closed_page_note_and_folder_leave_nothing(cx: &mut TestAppContext) {
    let (view, cx, fake) = studio(cx);
    let version = Cell::new(1);
    closes_clean(&view, cx, |cx| {
        let kinds = [
            ItemKind::Browser { url: format!("http://127.0.0.1:5173/{}", version.get()) },
            ItemKind::Note { text: "Release\n- [ ] tag".into() },
            ItemKind::Folder { path: "/w/proj".into() },
        ];
        for kind in kinds {
            let tile = arrives(&view, cx, &fake, kind, next(&version));
            close(&view, cx, tile);
        }
    });
}

/// The palette opened, typed into and dismissed; the window picker and a tile's name field the
/// same; the overview opened and closed.
#[gpui::test]
fn the_overlays_and_the_overview_leave_nothing(cx: &mut TestAppContext) {
    let (view, cx, _fake) = studio(cx);
    closes_clean(&view, cx, |cx| {
        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.picker.is_some()), "the picker is up");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.picker.is_none()), "Esc dismissed it");
        cx.simulate_keystrokes("cmd-e");
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.rename.is_some()), "the name field is up");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.rename.is_none()), "Esc closed it");
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.palette.is_some()), "the palette is up");
        cx.simulate_input("term");
        cx.run_until_parked();
        cx.simulate_keystrokes("escape escape");
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.palette.is_none()), "Esc dismissed it");
        cx.simulate_keystrokes("cmd-alt-o");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-alt-o");
        cx.run_until_parked();
    });
}

/// An agent's thread shown, fed and hidden, then its shell closed and ended.
#[gpui::test]
fn a_closed_agent_with_its_thread_leaves_nothing(cx: &mut TestAppContext) {
    let (view, cx, mut fake) = studio(cx);
    let version = Cell::new(1);
    closes_clean(&view, cx, |cx| {
        let session = SessionId::new();
        let tile = opens(&view, cx, &fake, session, fake.me, next(&version));
        view.update_in(cx, |v, _w, cx| {
            v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
            v.focus_tile(tile, cx);
        });
        cx.run_until_parked();
        let thread = agent_thread(&view, cx, fake.key, session);
        let mut state = crate::conversation::thread::fixtures::thread("edit");
        state.meta.id = thread;
        state.meta.terminal = Some(session);
        let snapshot =
            ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 40 }, state: Box::new(state) };
        view.update_in(cx, |v, _w, cx| v.thread_frame(fake.key, thread, snapshot, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-j");
        cx.run_until_parked();
        let asked: Vec<ClientMsg> = fake.drain();
        assert!(
            asked.iter().any(|m| matches!(
                m,
                ClientMsg::Thread(ThreadRequest::Unfollow { thread: t }) if *t == thread
            )),
            "{asked:?}"
        );
        close(&view, cx, tile);
        cx.executor().advance_clock(UNDO_CLOSE);
        cx.run_until_parked();
        view.update_in(cx, |v, _w, cx| v.session_closed(session, cx));
    });
}

/// A second worker with a tile of every kind open is forgotten: its views, streams and entries
/// go with it.
#[gpui::test]
fn a_removed_worker_with_open_tiles_leaves_nothing(cx: &mut TestAppContext) {
    let (view, cx, _studio) = studio(cx);
    let seed = Cell::new(1);
    closes_clean(&view, cx, |cx| {
        let laptop = connect(&view, cx, u128::from(next(&seed)), "laptop");
        let key = laptop.key;
        let session = SessionId::new();
        opens(&view, cx, &laptop, session, laptop.me, 1);
        view.update_in(cx, |v, _w, cx| v.term_event(session, frame(&["~ % ls"]), cx));
        let window = slopty_core::WindowId(7);
        arrives(&view, cx, &laptop, ItemKind::Window { window }, 2);
        view.update_in(cx, |v, _w, cx| {
            let opened = ScreenEvent::Opened {
                stream: StreamId(1),
                target: CaptureTarget::Window(window),
                codec: slopty_proto::screen::VideoCodec::Hevc,
                width: 1280,
                height: 800,
                scale: 2.0,
                stripes: Vec::new(),
            };
            v.screen_event(key, opened, cx);
        });
        arrives(&view, cx, &laptop, ItemKind::Note { text: "plan".into() }, 3);
        arrives(&view, cx, &laptop, ItemKind::File { path: "/w/main.rs".into() }, 4);
        let page = arrives(
            &view,
            cx,
            &laptop,
            ItemKind::Browser { url: "http://127.0.0.1:3000/".into() },
            5,
        );
        arrives(&view, cx, &laptop, ItemKind::Folder { path: "/w".into() }, 6);
        assert!(view.read_with(cx, |v, _| v.browser(page.item).is_some()), "the page is open");
        // The page goes with the worker at once, not at the next draw, which a hidden window
        // never makes: its web view holds the worker's data store, which a forgotten worker's
        // pages must give up.
        view.update_in(cx, |v, _w, cx| {
            v.remove_worker(key, cx);
            assert!(v.browser(page.item).is_none(), "the page went before any draw");
        });
        cx.run_until_parked();
    });
}

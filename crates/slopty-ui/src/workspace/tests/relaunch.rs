//! What a relaunch puts back beside the arrangement: the main window's frame, the face or TUI
//! each agent's tile showed, and the tiles out in windows of their own.

use slopty_client::layout::WindowFrame;
use slopty_core::WindowId;
use slopty_proto::screen::VideoCodec;

use super::*;
use crate::workspace::popout::PopOutView;

/// A workspace that saves to `path`, as the last run left it there.
fn relaunched<'a>(
    cx: &'a mut TestAppContext,
    path: &std::path::Path,
) -> (Entity<WorkspaceView>, &'a mut VisualTestContext) {
    let saved = read_layout(path).ok().flatten().expect("written");
    let path = path.to_owned();
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(key_bindings());
        cx.bind_keys(crate::terminal::key_bindings());
    });
    let (view, cx) = cx.add_window_view(move |window, cx| {
        let mut view = WorkspaceView::new(Theme::default(), Some(saved), cx);
        view.set_animation(false);
        view.set_layout_path(path);
        window.focus(&view.focus, cx);
        view
    });
    cx.simulate_resize(size(px(VIEWPORT.0), px(VIEWPORT.1)));
    cx.run_until_parked();
    (view, cx)
}

/// A worker `name` back after the relaunch with `items` in its registry and `sessions` running.
fn back(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    seed: u128,
    sessions: Vec<SessionSummary>,
    items: Vec<Item>,
) -> Fake {
    let (tx, rx) = mpsc::channel(256);
    let me = ClientId::new();
    let key = WorkerKey::new(seed);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    view.update_in(cx, |v, _window, cx| {
        v.add_worker(key, "studio".to_owned(), cx);
        let link = WorkerLink { me, out: tx, open_screen: factory, remote: None };
        v.connect_worker(key, link, hello("studio", sessions), cx);
        v.apply_sync(key, ItemSync::Snapshot { version: 1, items }, cx);
    });
    cx.run_until_parked();
    Fake { key, me, rx }
}

fn layout_file() -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("slopty-relaunch-{}", ItemId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("layout.json");
    (dir, path)
}

/// The TUI the person picked over an agent's thread, and where the main window stood, are saved
/// with the layout; the next run shows that tile's TUI again once its worker is back with the
/// thread, and says where the window goes.
#[gpui::test]
fn a_relaunch_shows_each_picked_face_and_knows_where_the_window_stood(cx: &mut TestAppContext) {
    let (dir, path) = layout_file();
    let session = SessionId::new();
    let frame = WindowFrame {
        display: None,
        x: 40.0,
        y: 30.0,
        width: 1100.0,
        height: 760.0,
        fullscreen: false,
    };
    // Its agent is known by its thread ([`agent_thread`]), as every agent is.
    let running = summary(session, None);
    let item = {
        let (view, vcx) = workspace(cx);
        view.update(vcx, |v, _| v.set_layout_path(path.clone()));
        let studio = connect(&view, vcx, 1, "studio");
        let tile = opens(&view, vcx, &studio, session, studio.me, 1);
        let key = studio.key;
        view.update_in(vcx, |v, _w, cx| {
            v.session_opened(key, running.clone(), cx);
            v.focus_tile(tile, cx);
            v.set_window_frame(Some(frame.clone()), cx);
        });
        vcx.run_until_parked();
        agent_thread(&view, vcx, key, session);
        assert!(view.read_with(vcx, |v, _| v.face_shown(session)), "the thread first");
        vcx.simulate_keystrokes("cmd-j");
        vcx.run_until_parked();
        assert!(!view.read_with(vcx, |v, _| v.face_shown(session)), "the TUI, picked");
        vcx.executor().advance_clock(SAVE_AFTER);
        vcx.run_until_parked();
        view.read_with(vcx, |v, _| v.item(tile).cloned()).expect("its item")
    };
    let saved = read_layout(&path).ok().flatten().expect("written");
    assert_eq!(saved.window.as_ref(), Some(&frame));
    assert_eq!(saved.faces.len(), 1, "{:?}", saved.faces);

    let (view, cx) = relaunched(cx, &path);
    assert_eq!(view.read_with(cx, |v, _| v.window_frame().cloned()), Some(frame));
    let studio = back(&view, cx, 1, vec![running], vec![item]);
    agent_thread(&view, cx, studio.key, session);
    view.update(cx, |_v, cx| cx.notify());
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.face_shown(session)), "the TUI again");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A tile out in a window of its own when the app quit goes back out when its stream opens
/// after the relaunch, where that window stood.
#[gpui::test]
fn a_relaunch_puts_a_popped_out_tile_back_in_its_window(cx: &mut TestAppContext) {
    let (dir, path) = layout_file();
    let window = WindowId(7);
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::Window { window },
        name: None,
        facts: BTreeMap::new(),
    };
    let tile = TileRef { worker: WorkerKey::new(1), item: item.id };
    let frame = WindowFrame {
        display: None,
        x: 60.0,
        y: 70.0,
        width: 640.0,
        height: 400.0,
        fullscreen: false,
    };
    let mut saved = Layout::new(LayoutConfig::default());
    saved.open(
        tile,
        slopty_client::layout::Placement::Remote {
            home: slopty_client::layout::GroupKey::machine(tile.worker),
        },
    );
    let mut saved = saved.save();
    saved.popouts.push(slopty_client::layout::SavedPopout { tile, frame });
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();

    let (view, cx) = relaunched(cx, &path);
    let studio = back(&view, cx, 1, Vec::new(), vec![item]);
    let opened = ScreenEvent::Opened {
        stream: StreamId(1),
        target: CaptureTarget::Window(window),
        codec: VideoCodec::Hevc,
        width: 1280,
        height: 800,
        scale: 2.0,
        stripes: Vec::new(),
    };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.screen_event(key, opened, cx));
    cx.run_until_parked();
    let popped = cx.update(|_, cx| {
        cx.windows().iter().find_map(gpui::AnyWindowHandle::downcast::<PopOutView>)
    });
    let popped = popped.expect("out in its own window again");
    let size = popped
        .update(cx, |_p, window, _cx| window.window_bounds().get_bounds().size)
        .expect("open");
    #[expect(clippy::cast_possible_truncation, reason = "whole points")]
    let size = (f32::from(size.width) as i32, f32::from(size.height) as i32);
    assert_eq!(size, (640, 400), "where it stood");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// How far this device read a project's timeline is saved with the layout, so the board's recap
/// after a relaunch starts where the last run left off rather than from the top.
#[gpui::test]
fn a_relaunch_keeps_how_far_each_board_was_read(cx: &mut TestAppContext) {
    use crate::project::recap::Looked;
    let (dir, path) = layout_file();
    let project = slopty_proto::project::ProjectId::new("release").expect("a name");
    let read = Looked { seq: 42, at_ms: WallMs::from_millis(1_700_000_000_000) };
    {
        let (view, vcx) = workspace(cx);
        view.update(vcx, |v, _| {
            v.set_layout_path(path.clone());
            v.restore_projects_looked([(project.clone(), read)]);
            v.save_layout_now();
        });
        vcx.run_until_parked();
    }
    let saved = read_layout(&path).ok().flatten().expect("written");
    assert_eq!(saved.looked.len(), 1, "{:?}", saved.looked);

    let (view, cx) = relaunched(cx, &path);
    let back = view.read_with(cx, |v, _| v.projects_looked());
    assert_eq!(back, [(project, read)], "the cursor came back");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A cold launch draws a worker's tiles from its items as this device last saw them, each under
/// the pill saying the worker is away, rather than holes; its first snapshot then replaces
/// them, and a tile whose item it no longer has leaves.
#[gpui::test]
fn a_cold_launch_draws_the_kept_tiles_until_the_worker_is_back(cx: &mut TestAppContext) {
    let (dir, path) = layout_file();
    let items = dir.join("items");
    let (shell, page) = {
        let (view, vcx) = workspace(cx);
        view.update(vcx, |v, _| {
            v.set_layout_path(path.clone());
            v.set_item_cache(items.clone());
        });
        let studio = connect(&view, vcx, 1, "studio");
        let shell = opens(&view, vcx, &studio, SessionId::new(), studio.me, 1);
        let page = ItemKind::Browser { url: "http://localhost:3000/".into() };
        let page = arrives(&view, vcx, &studio, page, 2);
        vcx.executor().advance_clock(SAVE_AFTER);
        vcx.run_until_parked();
        (shell, page)
    };

    let (view, cx) = relaunched(cx, &path);
    let key = shell.worker;
    view.update_in(cx, |v, _w, cx| {
        v.set_item_cache(items.clone());
        v.add_worker(key, "studio".to_owned(), cx);
    });
    cx.run_until_parked();
    for tile in [shell, page] {
        let id = tile.item.as_uuid();
        assert!(cx.debug_bounds(format!("item-{id}").leak()).is_some(), "drawn, not a hole");
        assert!(cx.debug_bounds(format!("state-{id}").leak()).is_some(), "saying it is away");
    }

    let kept = view.read_with(cx, |v, _| v.item(page).cloned()).expect("the page, kept");
    let _studio = back(&view, cx, 1, Vec::new(), vec![kept]);
    let (shell_gone, page_kept) =
        view.read_with(cx, |v, _| (!v.layout.contains(shell), v.layout.contains(page)));
    assert!(shell_gone, "the snapshot no longer has the shell: its tile leaves");
    assert!(page_kept);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A tile the device kept nothing of (the cache unset, or unreadable by this build) is still
/// drawn while its worker is away: the worker's name over the words saying where it is, in the
/// middle of its empty body. A worker
/// on another build offers its Update there, and once it is back without the item the tile
/// leaves.
#[gpui::test]
fn a_tile_kept_nowhere_says_where_its_worker_is(cx: &mut TestAppContext) {
    use std::cell::RefCell;
    use std::rc::Rc;

    use slopty_client::update::{Of, UpdateNotice};

    let (dir, path) = layout_file();
    let shell = {
        let (view, vcx) = workspace(cx);
        view.update(vcx, |v, _| v.set_layout_path(path.clone()));
        let studio = connect(&view, vcx, 1, "studio");
        let shell = opens(&view, vcx, &studio, SessionId::new(), studio.me, 1);
        vcx.executor().advance_clock(SAVE_AFTER);
        vcx.run_until_parked();
        shell
    };

    let (view, cx) = relaunched(cx, &path);
    let key = shell.worker;
    view.update_in(cx, |v, _w, cx| v.add_worker(key, "studio".to_owned(), cx));
    cx.run_until_parked();
    let id = shell.item.as_uuid();
    let item = Box::leak(format!("item-{id}").into_boxed_str());
    assert!(view.read_with(cx, |v, _| v.item(shell).is_none()), "nothing kept");
    assert!(cx.debug_bounds(item).is_some(), "drawn, not a hole");
    assert!(cx.debug_bounds(format!("missing-{id}").leak()).is_some());
    let state = cx.debug_bounds(format!("state-{id}").leak()).expect("saying it is away");
    let body = cx.debug_bounds(format!("missing-{id}").leak()).expect("its body");
    // Nothing else in the body: what is so stands in its middle, not at its foot.
    assert!((state.center().y - body.center().y).abs() < px(1.0), "{state:?} in {body:?}");
    assert!(cx.debug_bounds(format!("state-centred-{id}").leak()).is_some());

    let asked: Rc<RefCell<Vec<String>>> = Rc::default();
    let seen = Rc::clone(&asked);
    cx.update(|_w, cx| {
        let start: crate::add_worker::Update =
            Rc::new(move |host: &str, _w, _cx| seen.borrow_mut().push(host.to_owned()));
        cx.set_global(crate::add_worker::Updates { start: Some(start), ..Default::default() });
    });
    let notice =
        UpdateNotice { of: Of::Worker, host: "studio.ts.net".to_owned(), peer: "0.0.9".to_owned() };
    view.update_in(cx, |v, _w, cx| v.disconnect_worker(key, WorkerStatus::NeedsUpdate(notice), cx));
    cx.run_until_parked();
    let update = cx.debug_bounds(format!("update-worker-{id}").leak()).expect("Update offered");
    cx.simulate_click(update.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(*asked.borrow(), ["studio.ts.net"], "the placeholder's Update");

    let _studio = back(&view, cx, 1, Vec::new(), Vec::new());
    assert!(view.read_with(cx, |v, _| !v.layout.contains(shell)), "back without it: it leaves");
    assert!(cx.debug_bounds(item).is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

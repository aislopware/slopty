//! The workspace in a headless GPUI window: real layout, real key, mouse and scroll dispatch,
//! no process, no permissions, no pixels. Each worker is a channel: the test reads what the
//! workspace sends and feeds back what a worker would.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Entity, Modifiers, Pixels, Point, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
    VisualTestContext, point, px, size,
};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::{ClientId, ItemId, SessionId, StreamId};
use slopty_grid::{Cursor, Line, LineIndex, RowUpdate, SemanticMark, Style, TermModes};
use slopty_proto::ClientMsg;
use slopty_proto::agent::{
    AgentEvent, AgentKind, AgentSource, AgentStatus, BlockReason, SessionAgent,
};
use slopty_proto::items::{Item, ItemKind, ItemOp, ItemSync};
use slopty_proto::screen::{CaptureTarget, ScreenEvent, ScreenRequest};
use slopty_proto::terminal::{
    Frame, OpenSession, SessionState, SessionSummary, TermEvent, TermRequest,
};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use super::*;
use crate::screen::ScreenFactory;

const VIEWPORT: (f32, f32) = (1200.0, 800.0);

/// One fake worker: its key, this client's id there, and what the workspace sent it.
struct Fake {
    key: WorkerKey,
    me: ClientId,
    rx: mpsc::Receiver<ClientMsg>,
}

impl Fake {
    fn drain(&mut self) -> Vec<ClientMsg> {
        std::iter::from_fn(|| self.rx.try_recv().ok()).collect()
    }
}

/// A focused workspace, drawn once so the strip's size is known.
fn workspace(cx: &mut TestAppContext) -> (Entity<WorkspaceView>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(key_bindings());
        cx.bind_keys(crate::terminal::key_bindings());
    });
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut view = WorkspaceView::new(Theme::default(), None, cx);
        // A headless frame is a step, not a moment: the springs are unit-tested in
        // `slopty_client::layout` on a clock of their own; these tests assert where things land.
        view.set_animation(false);
        window.focus(&view.focus, cx);
        view
    });
    cx.simulate_resize(size(px(VIEWPORT.0), px(VIEWPORT.1)));
    cx.run_until_parked();
    (view, cx)
}

/// A worker `name` connects, its registry empty. The first snapshot of an empty workspace
/// asks for a shell; that request is drained.
fn connect(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    seed: u128,
    name: &str,
) -> Fake {
    let (tx, rx) = mpsc::channel(256);
    let me = ClientId::new();
    let key = WorkerKey::new(seed);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    view.update_in(cx, |v, _window, cx| {
        v.add_worker(key, name.to_owned(), cx);
        v.connect_worker(
            key,
            name.to_owned(),
            WorkerLink { me, out: tx, open_screen: factory, remote: None },
            Vec::new(),
            cx,
        );
        v.apply_sync(key, ItemSync::Snapshot { version: 0, items: Vec::new() }, cx);
    });
    cx.run_until_parked();
    let mut fake = Fake { key, me, rx };
    fake.drain();
    fake
}

fn summary(session: SessionId, cwd: Option<&str>) -> SessionSummary {
    SessionSummary {
        id: session,
        title: "shell".into(),
        cwd: cwd.map(str::to_owned),
        repo: None,
        cols: 80,
        rows: 24,
        state: SessionState::Running,
        viewers: 1,
        command: Vec::new(),
        agent: None,
    }
}

/// The worker opened `session` for `by` (this client when it is `fake.me`) and made its item.
fn opens(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    session: SessionId,
    by: ClientId,
    version: u64,
) -> TileRef {
    opens_in(view, cx, fake, session, by, version, None)
}

#[expect(clippy::too_many_arguments, reason = "a test fixture, not an interface")]
fn opens_in(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    session: SessionId,
    by: ClientId,
    version: u64,
    cwd: Option<&str>,
) -> TileRef {
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::Terminal { session },
        sleeping: false,
        name: None,
    };
    let tile = TileRef { worker: fake.key, item: item.id };
    let key = fake.key;
    view.update_in(cx, |v, _window, cx| {
        v.session_opened(key, summary(session, cwd), cx);
        v.apply_sync(key, ItemSync::Delta { version, by, op: ItemOp::Upsert(item) }, cx);
    });
    cx.run_until_parked();
    tile
}

/// An item from elsewhere (a note another client made, a window).
fn arrives(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    kind: ItemKind,
    version: u64,
) -> TileRef {
    let item = Item { id: ItemId::new(), kind, sleeping: false, name: None };
    let tile = TileRef { worker: fake.key, item: item.id };
    let key = fake.key;
    view.update_in(cx, |v, _window, cx| {
        let by = ClientId::new();
        v.apply_sync(key, ItemSync::Delta { version, by, op: ItemOp::Upsert(item) }, cx);
    });
    cx.run_until_parked();
    tile
}

fn frame(rows: &[&str]) -> TermEvent {
    TermEvent::Frame(Frame {
        seq: 1,
        full: true,
        epoch: 0,
        cols: 80,
        rows: u16::try_from(rows.len()).unwrap(),
        cursor: Cursor::default(),
        modes: TermModes::empty(),
        oldest_line: LineIndex(0),
        first_visible_line: LineIndex(0),
        total_lines: rows.len() as u64,
        input_ack: 0,
        images: Vec::new(),
        updates: rows
            .iter()
            .enumerate()
            .map(|(row, text)| RowUpdate {
                row: u16::try_from(row).unwrap(),
                line: Line::from_text(text, 80, Style::DEFAULT),
            })
            .collect(),
    })
}

fn marked_frame(seq: u64, rows: &[(&str, SemanticMark)], cursor_row: u16) -> TermEvent {
    TermEvent::Frame(Frame {
        seq,
        full: seq == 1,
        epoch: 0,
        cols: 80,
        rows: u16::try_from(rows.len()).unwrap(),
        cursor: Cursor { row: cursor_row, ..Cursor::default() },
        modes: TermModes::empty(),
        oldest_line: LineIndex(0),
        first_visible_line: LineIndex(0),
        total_lines: rows.len() as u64,
        input_ack: 0,
        images: Vec::new(),
        updates: rows
            .iter()
            .enumerate()
            .map(|(row, (text, mark))| {
                let mut line = Line::from_text(text, 80, Style::DEFAULT);
                line.mark = *mark;
                RowUpdate { row: u16::try_from(row).unwrap(), line }
            })
            .collect(),
    })
}

/// `debug_bounds` wants a static selector; tests may leak a handful.
fn selector(part: &str, id: ItemId) -> &'static str {
    Box::leak(format!("{part}-{}", id.as_uuid()).into_boxed_str())
}

fn terminal_focused(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    session: SessionId,
) -> bool {
    cx.update(|window, cx| {
        view.read(cx)
            .terminal(session)
            .is_some_and(|t| t.read(cx).focus_handle(cx).is_focused(window))
    })
}

fn focused(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Option<TileRef> {
    view.read_with(cx, |v, _| v.focused())
}

fn column_of(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> usize {
    view.read_with(cx, |v, _| v.layout().position(tile).map(|p| p.column)).expect("placed")
}

/// Three shells this client opened, one after the other: three columns, the last focused.
fn three_shells(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
) -> [(SessionId, TileRef); 3] {
    let mut out = Vec::new();
    for version in 1..=3 {
        let session = SessionId::new();
        out.push((session, opens(view, cx, fake, session, fake.me, version)));
    }
    [out[0], out[1], out[2]]
}

/// A two-finger swipe on the trackpad at `at`: began, `steps` moves of `(dx, dy)`, ended,
/// then (optionally) a macOS momentum tail after the fingers lift.
fn swipe(
    cx: &mut VisualTestContext,
    at: Point<Pixels>,
    (dx, dy): (f32, f32),
    steps: u8,
    momentum: u8,
) {
    let event = |phase, dx: f32, dy: f32| ScrollWheelEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
        modifiers: Modifiers::default(),
        touch_phase: phase,
    };
    cx.simulate_event(event(TouchPhase::Started, 0.0, 0.0));
    for _ in 0..steps {
        cx.simulate_event(event(TouchPhase::Moved, dx, dy));
    }
    cx.simulate_event(event(TouchPhase::Ended, 0.0, 0.0));
    for _ in 0..momentum {
        cx.simulate_event(event(TouchPhase::Moved, dx, dy));
    }
    cx.run_until_parked();
}

// ----- the pure pieces ---------------------------------------------------------------------

#[test]
fn a_note_is_titled_by_its_first_line() {
    assert_eq!(note_title(""), "note");
    assert_eq!(note_title("\n\n  # Plan  \nmore"), "Plan");
    assert_eq!(note_title("- [ ] ship it\n- [x] test it"), "ship it · 1/2");
    let long = "a".repeat(NOTE_TITLE_CHARS + 5);
    assert_eq!(note_title(&long).chars().count(), NOTE_TITLE_CHARS + 1, "cut, with an ellipsis");
    assert_eq!(note_progress("no tasks"), None);
}

#[test]
fn a_file_card_is_titled_by_its_name_and_directory() {
    assert_eq!(file_title("/w/src/main.rs"), "main.rs · src");
    assert_eq!(file_title("main.rs"), "main.rs");
    assert_eq!(file_title("/etc/"), "etc");
}

/// A file tile asks the worker for its text, takes the keyboard when focused, sends ⌘S as a
/// `WriteFile` to its own worker, marks the header while the edit is unsaved, and hears how
/// the write went.
#[gpui::test]
fn a_file_tile_is_edited_and_saved_through_its_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let path = "/w/notes.md";
    let tile = arrives(&view, cx, &studio, ItemKind::File { path: path.to_owned() }, 1);
    assert!(
        studio.drain().iter().any(|m| matches!(m, ClientMsg::ReadFile { path: p } if p == path)),
        "the tile reads its file"
    );
    let text = slopty_proto::file::FileRead::Text {
        text: "# Notes".to_owned(),
        more_lines: 0,
        size: 8,
        modified_ms: 1_000,
        final_newline: true,
    };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.file_read(key, path, &text, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    cx.simulate_input("x");
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("unsaved", tile.item)).is_some(), "the header says so");
    cx.simulate_keystrokes("cmd-s");
    cx.run_until_parked();
    let writes: Vec<ClientMsg> =
        studio.drain().into_iter().filter(|m| matches!(m, ClientMsg::WriteFile { .. })).collect();
    assert_eq!(
        writes,
        [ClientMsg::WriteFile {
            path: path.to_owned(),
            text: "x# Notes\n".to_owned(),
            base_modified_ms: Some(1_000),
        }]
    );
    let saved = slopty_proto::file::WriteResult::Saved { size: 9, modified_ms: 2_000 };
    view.update_in(cx, |v, _w, cx| v.file_written(key, path, &saved, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("unsaved", tile.item)).is_none(), "saved: the dot goes");
}

#[test]
fn a_finished_badge_says_the_status_and_the_time() {
    let done = |exit| Finished { command: "make".into(), exit, elapsed: Duration::from_secs(3) };
    assert!(done(Some(0)).label().starts_with("Done · "), "{}", done(Some(0)).label());
    assert!(done(Some(2)).label().starts_with("Exit 2 · "), "{}", done(Some(2)).label());
}

#[test]
fn a_banner_is_led_by_the_tiles_name() {
    assert_eq!(banner_title(Some("api"), "needs you"), "api · needs you");
    assert_eq!(banner_title(None, "needs you"), "needs you");
    let (title, body) = program_banner(None, "", "hi");
    assert!(!title.is_empty(), "a program's banner always has a title");
    assert_eq!(body, "hi");
}

// ----- opening and placing -----------------------------------------------------------------

/// ⌘T asks the focused tile's worker for a shell; the echo of that item opens a column right
/// of the focused one, focuses it, and the terminal takes the keyboard. A second ⌘T starts in
/// the focused shell's directory.
#[gpui::test]
fn cmd_t_asks_the_worker_for_a_shell_and_its_echo_opens_a_focused_column(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    assert!(cx.debug_bounds("workspace").is_some(), "the workspace is drawn");

    cx.simulate_keystrokes("cmd-t");
    let sent = fake.drain();
    assert!(
        matches!(sent.as_slice(), [ClientMsg::OpenSession(OpenSession { cwd: None, .. })]),
        "no shell to inherit from: {sent:?}"
    );
    let session = SessionId::new();
    let tile = opens_in(&view, cx, &fake, session, fake.me, 1, Some("/tmp/work"));
    assert_eq!(focused(&view, cx), Some(tile));
    assert!(terminal_focused(&view, cx, session), "the terminal has the keyboard");
    assert!(cx.debug_bounds(selector("item", tile.item)).is_some(), "drawn");
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(
            m,
            ClientMsg::Term { session: s, req: TermRequest::Attach { .. } } if *s == session
        )),
        "it attached: {sent:?}"
    );

    cx.simulate_keystrokes("cmd-t");
    let sent = fake.drain();
    assert!(
        matches!(
            sent.as_slice(),
            [ClientMsg::OpenSession(OpenSession { cwd: Some(cwd), command, .. })]
                if cwd == "/tmp/work" && command.is_empty()
        ),
        "{sent:?}"
    );
    cx.simulate_keystrokes("cmd-shift-t");
    let sent = fake.drain();
    assert!(
        matches!(
            sent.as_slice(),
            [ClientMsg::OpenSession(OpenSession { command, .. })]
                if command == &[AGENT_COMMAND.to_owned()]
        ),
        "{sent:?}"
    );
    let second = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    assert_eq!(
        column_of(&view, cx, second),
        column_of(&view, cx, tile).saturating_add(1),
        "right of it"
    );
    assert_eq!(focused(&view, cx), Some(second));
}

/// What another client opens lands at the end of the strip that holds that worker's tiles,
/// and the focus stays where the human is.
#[gpui::test]
fn an_item_from_elsewhere_joins_the_end_without_taking_the_focus(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), (_, second), _] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    let theirs = opens(&view, cx, &fake, SessionId::new(), ClientId::new(), 4);
    assert_eq!(focused(&view, cx), Some(first), "the focus stayed");
    assert_eq!(column_of(&view, cx, theirs), 3, "at the end");
    assert_eq!(column_of(&view, cx, second), 1);
}

/// Tiles of two workers share one layout: a second worker's first tile gets a workspace of
/// its own (the first one is not empty), and a tile knows which worker it is.
#[gpui::test]
fn two_workers_share_one_layout(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let a = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let b = opens(&view, cx, &laptop, SessionId::new(), ClientId::new(), 1);
    view.read_with(cx, |v, _| {
        let (pa, pb) = (v.layout().position(a).unwrap(), v.layout().position(b).unwrap());
        assert_ne!(pa.workspace, pb.workspace, "a new workspace for the laptop's first tile");
        assert_eq!(v.len(), 2);
        assert_eq!(b.worker, laptop.key);
    });
    // A tile the laptop's shell opens for this client lands beside the focus, whoever's.
    let c = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 2);
    assert_eq!(focused(&view, cx), Some(c));
    let (pa, pc) = view
        .read_with(cx, |v, _| (v.layout().position(a).unwrap(), v.layout().position(c).unwrap()));
    assert_eq!(pa.workspace, pc.workspace, "local opens go where the human is");
}

/// A worker that drops keeps its tiles, which say it is away; the titlebar names it, and only
/// it. When it comes back, a tile whose item is gone from its snapshot leaves, the others stay.
#[gpui::test]
fn a_lost_worker_keeps_its_tiles_until_its_snapshot_says_otherwise(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let kept = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let gone = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    let _theirs = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 1);
    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.run_until_parked();
    let studio_down = "down-00000000000000000000000000000001";
    let laptop_down = "down-00000000000000000000000000000002";
    assert!(cx.debug_bounds(studio_down).is_none(), "nobody is down");

    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert!(v.layout().contains(kept) && v.layout().contains(gone), "the tiles stay");
    });
    assert!(cx.debug_bounds(studio_down).is_some(), "the titlebar names the worker that is down");
    assert!(cx.debug_bounds(laptop_down).is_none(), "and not the one that is up");
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(
        tree.iter().any(|n| n.label.as_deref().is_some_and(|l| l.starts_with("studio, lost"))),
        "{tree:#?}"
    );

    // Back, with only `kept` in its registry.
    let (tx, _rx) = mpsc::channel(64);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    let item = view.read_with(cx, |v, _| v.item(kept).cloned()).unwrap();
    view.update_in(cx, |v, _w, cx| {
        let link = WorkerLink { me: studio.me, out: tx, open_screen: factory, remote: None };
        v.connect_worker(key, "studio".into(), link, Vec::new(), cx);
        v.apply_sync(key, ItemSync::Snapshot { version: 9, items: vec![item] }, cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert!(v.layout().contains(kept), "still in the registry, still a tile");
        assert!(!v.layout().contains(gone), "gone from the registry, gone from the layout");
    });
}

// ----- the keyboard ------------------------------------------------------------------------

/// The niri keys move the focus and the columns: ⌘⌥←/→ step, ⌘1 jumps, ⌘⌥⇧← carries the
/// column, ⌘] tucks a tile into the column on its right, and the keyboard follows the focus
/// into each terminal.
#[gpui::test]
fn the_layout_keys_move_the_focus_and_the_columns(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(s1, first), (s2, second), (_s3, third)] = three_shells(&view, cx, &fake);
    assert_eq!(focused(&view, cx), Some(third));

    cx.simulate_keystrokes("cmd-alt-left");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(second));
    assert!(terminal_focused(&view, cx, s2), "the keyboard followed");

    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(first));
    assert!(terminal_focused(&view, cx, s1));

    cx.simulate_keystrokes("cmd-alt-shift-right");
    cx.run_until_parked();
    assert_eq!(column_of(&view, cx, first), 1, "carried right");
    assert_eq!(column_of(&view, cx, second), 0);
    assert_eq!(focused(&view, cx), Some(first), "and still focused");

    cx.simulate_keystrokes("cmd-]");
    cx.run_until_parked();
    let (pf, pt) = view.read_with(cx, |v, _| {
        (v.layout().position(first).unwrap(), v.layout().position(third).unwrap())
    });
    assert_eq!(pf.column, pt.column, "consumed into the column on the right");
    cx.simulate_keystrokes("cmd-alt-up");
    cx.run_until_parked();
    assert_ne!(focused(&view, cx), Some(first), "⌘⌥↑ walks the column");
}

/// ⌘R cycles the column through the preset widths; the terminal's grid follows the width it
/// comes to rest at. ⌘⇧↩ fills the view, and again restores.
#[gpui::test]
fn the_width_keys_resize_the_column_and_its_grid(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, fake.me, 1);
    let width = |cx: &mut VisualTestContext| {
        cx.debug_bounds(selector("item", tile.item)).map(|b| f32::from(b.size.width)).unwrap()
    };
    let cols = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| v.terminal(session).unwrap().read(cx).size().cols)
    };
    let strip = f32::from(cx.debug_bounds("strip").unwrap().size.width);
    let half = width(cx);
    assert!((half - strip / 2.0).abs() < 2.0, "half the strip, edge to edge: {half}");
    let half_cols = cols(cx);
    cx.simulate_keystrokes("cmd-r");
    cx.run_until_parked();
    let wider = width(cx);
    assert!(wider > half + 100.0, "⌘R: two thirds, {half} → {wider}");
    assert!(cols(cx) > half_cols, "the grid grew with it");
    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
    assert!(width(cx) > strip - 40.0, "maximized: {}", width(cx));
    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
    assert!((width(cx) - wider).abs() < 2.0, "and back");
}

/// ⌘-/⌘= change the terminal text size (not a zoom): the theme's mono size moves, within its
/// bounds, and ⌘0 puts it back.
#[gpui::test]
fn cmd_plus_and_minus_change_the_terminal_text(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _fake = connect(&view, cx, 1, "studio");
    let base = view.read_with(cx, |v, _| v.theme().typography.mono_size);
    cx.simulate_keystrokes("cmd-=");
    cx.run_until_parked();
    assert!((view.read_with(cx, |v, _| v.theme().typography.mono_size) - base - 1.0).abs() < 1e-3);
    for _ in 0..60 {
        cx.simulate_keystrokes("cmd--");
    }
    cx.run_until_parked();
    let floor = view.read_with(cx, |v, _| v.theme().typography.mono_size);
    assert!(floor >= commands::FONT_MIN, "{floor}");
    cx.simulate_keystrokes("cmd-0");
    cx.run_until_parked();
    assert!((view.read_with(cx, |v, _| v.theme().typography.mono_size) - base).abs() < 1e-3);
}

// ----- the trackpad and the wheel ----------------------------------------------------------

/// A horizontal two-finger swipe drags the strip and snaps to a column when it ends: the
/// focus lands on the column in view. The momentum macOS sends after the lift is swallowed,
/// and never reaches the terminal under it.
#[gpui::test]
fn a_sideways_swipe_pages_the_strip_and_its_momentum_is_swallowed(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [_, _, (s3, third)] = three_shells(&view, cx, &fake);
    assert_eq!(focused(&view, cx), Some(third));
    let at = cx.debug_bounds(selector("item", third.item)).unwrap().center();
    // Fingers moving right: the content follows them right, towards the first column.
    swipe(cx, at, (60.0, 0.0), 20, 10);
    let now = focused(&view, cx).unwrap();
    assert_ne!(now, third, "the strip moved off the last column");
    let offset =
        view.read_with(cx, |v, cx| v.terminal(s3).map(|t| t.read(cx).state().view_offset()));
    assert_eq!(offset, Some(0), "the terminal under the swipe did not scroll");
}

/// A vertical swipe over a terminal is the terminal's: it scrolls into its history and the
/// strip holds still.
#[gpui::test]
fn a_vertical_swipe_over_a_terminal_scrolls_its_history(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, fake.me, 1);
    view.update_in(cx, |v, _w, cx| {
        let TermEvent::Frame(mut f) = frame(&["$ echo hi", "hi", ""]) else { return };
        f.first_visible_line = LineIndex(50);
        f.total_lines = 53;
        v.term_event(session, TermEvent::Frame(f), cx);
    });
    cx.run_until_parked();
    let before = cx.debug_bounds(selector("item", tile.item)).unwrap();
    swipe(cx, before.center(), (0.0, 30.0), 8, 0);
    let after = cx.debug_bounds(selector("item", tile.item)).unwrap();
    let offset =
        view.read_with(cx, |v, cx| v.terminal(session).unwrap().read(cx).state().view_offset());
    assert!(offset > 0, "the shell scrolled into its history");
    assert_eq!(after.origin, before.origin, "the strip stayed");
}

/// ⌘⌥ and the wheel step through the columns, one per notch.
#[gpui::test]
fn cmd_alt_wheel_steps_the_columns(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [_, (_, second), (_, third)] = three_shells(&view, cx, &fake);
    let at = cx.debug_bounds(selector("item", third.item)).unwrap().center();
    cx.simulate_event(ScrollWheelEvent {
        position: at,
        delta: ScrollDelta::Lines(point(1.0, 0.0)),
        modifiers: Modifiers { platform: true, alt: true, ..Modifiers::default() },
        touch_phase: TouchPhase::Moved,
    });
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(second), "one notch, one column left");
}

/// Dragging a tile's header onto the middle of another column puts it in that column.
#[gpui::test]
fn a_header_dragged_onto_a_column_joins_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), (_, second), _] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    let header = cx.debug_bounds(selector("title", first.item)).unwrap().center();
    let onto = cx.debug_bounds(selector("item", second.item)).unwrap().center();
    cx.simulate_mouse_down(header, gpui::MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        point(header.x + px(20.0), header.y),
        Some(gpui::MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_move(onto, Some(gpui::MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(onto, gpui::MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    let (pf, ps) = view.read_with(cx, |v, _| {
        (v.layout().position(first).unwrap(), v.layout().position(second).unwrap())
    });
    assert_eq!(pf.column, ps.column, "one column now: {pf:?} {ps:?}");
    assert_eq!(focused(&view, cx), Some(first), "the dragged tile keeps the focus");
}

// ----- drawing only what shows -------------------------------------------------------------

/// Only tiles near the view are drawn: a column two screens away has no element and its
/// terminal prepares no rows; bringing it into view draws it.
#[gpui::test]
fn tiles_far_from_the_view_are_not_drawn(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let mut tiles = Vec::new();
    for version in 1..=6 {
        tiles.push(opens(&view, cx, &fake, SessionId::new(), fake.me, version));
    }
    let first = tiles[0];
    assert!(cx.debug_bounds(selector("item", first.item)).is_none(), "far left: not drawn");
    let drawn = view.read_with(cx, |v, _| v.placed.len());
    assert!(drawn < tiles.len(), "{drawn} of {} drawn", tiles.len());
    let rows = cx.update(|_window, cx| crate::terminal::rows_prepared(cx));
    let per_grid = view.read_with(cx, |v, cx| {
        v.terminal(session_of(v, tiles[5])).and_then(|t| t.read(cx).metrics()).map(|m| m.rows)
    });
    assert!(
        per_grid.is_some_and(|r| rows <= usize::from(r).saturating_mul(drawn)),
        "only the drawn grids prepared rows: {rows}"
    );
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("item", first.item)).is_some(), "in view: drawn");
    assert!(cx.debug_bounds(selector("item", tiles[5].item)).is_none(), "far right now");
}

/// A shell's output redraws that shell's tile only: its neighbours are replayed from the view
/// cache. A neighbour whose zoom stopped moving is drawn afresh at the same bounds, since its
/// paint changed.
#[gpui::test]
fn output_redraws_its_own_tile_only(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, left), (busy, right), _] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(right, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("item", left.item)).is_some(), "the neighbour is drawn");
    let quiet = view.read_with(cx, |v, _| v.terminal(session_of(v, left)).cloned()).unwrap();
    let loud = view.read_with(cx, |v, _| v.terminal(busy).cloned()).unwrap();
    let renders = |cx: &mut VisualTestContext| {
        (quiet.read_with(cx, |t, _| t.renders()), loud.read_with(cx, |t, _| t.renders()))
    };
    let (quiet_before, loud_before) = renders(cx);
    for i in 0..3 {
        let line = format!("line {i}");
        view.update_in(cx, |v, _w, cx| v.term_event(busy, frame(&[&line, ""]), cx));
        cx.run_until_parked();
    }
    let (quiet_after, loud_after) = renders(cx);
    assert_eq!(quiet_after, quiet_before, "the quiet neighbour was replayed, not rendered");
    assert!(loud_after >= loud_before.saturating_add(3), "{loud_before} → {loud_after}");

    quiet.update(cx, |t, _| t.set_zooming(true));
    view.update_in(cx, |_v, _w, cx| cx.notify());
    cx.run_until_parked();
    assert_eq!(
        renders(cx).0,
        quiet_after.saturating_add(1),
        "the zoom settled: drawn afresh at the same bounds"
    );
}

/// A remote window scrolled off screen lets its stream go after the grace, and asks for it
/// again when it is back in view.
#[gpui::test]
fn an_offscreen_window_lets_its_stream_go_after_the_grace(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    view.update(cx, |v, _| v.stream_grace = Duration::ZERO);
    let window = slopty_core::WindowId(7);
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window }, 1);
    let open = |sent: &[ClientMsg]| {
        sent.iter().any(|m| matches!(m, ClientMsg::Screen(ScreenRequest::Open { .. })))
    };
    assert!(open(&fake.drain()), "a window in view asks for its stream");
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.screen_event(
            key,
            ScreenEvent::Opened {
                stream: StreamId(1),
                target: CaptureTarget::Window(window),
                codec: slopty_proto::screen::VideoCodec::Hevc,
                width: 1280,
                height: 800,
                scale: 2.0,
            },
            cx,
        );
    });
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.screen(tile.item).is_some()), "streaming");
    assert_eq!(cx.active_idle_sleep_preventions(), 1, "a streaming window holds the device");

    // Four shells of this client's push the window off to the left.
    for version in 2..=5 {
        opens(&view, cx, &fake, SessionId::new(), fake.me, version);
    }
    cx.executor().advance_clock(Duration::from_millis(10));
    cx.run_until_parked();
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let sent = fake.drain();
    assert!(
        sent.iter()
            .any(|m| matches!(m, ClientMsg::Screen(ScreenRequest::Close(s)) if *s == StreamId(1))),
        "off screen past the grace: let go, {sent:?}"
    );
    assert!(view.read_with(cx, |v, _| v.screen(tile.item).is_none()));

    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    assert!(open(&fake.drain()), "back in view: asked for again");
}

/// Listing the installed fonts is a synchronous trip to the font server: three shells drawn
/// at once list them once, not once per view.
#[gpui::test]
fn the_installed_fonts_are_listed_once_for_every_shell(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(shells[1].1, cx));
    cx.run_until_parked();
    for (_, tile) in shells {
        assert!(cx.debug_bounds(selector("item", tile.item)).is_some(), "every shell is drawn");
    }
    let picks = cx.update(|_window, cx| crate::terminal::family_picks(cx));
    assert_eq!(picks, 1, "one walk of the installed fonts for the whole app");
}

fn session_of(v: &WorkspaceView, tile: TileRef) -> SessionId {
    match v.item(tile).map(|i| &i.kind) {
        Some(ItemKind::Terminal { session }) => *session,
        _ => SessionId::new(),
    }
}

// ----- closing and taking back -------------------------------------------------------------

/// ⌘W on a shell whose command runs sends nothing until its bar is confirmed with ↩; Esc
/// keeps it. Confirmed, the tile goes at once and the session after the undo window.
#[gpui::test]
fn a_busy_shell_closes_only_when_confirmed(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, fake.me, 1);
    let prompt = SemanticMark::Prompt { exit: None, input: Some(2) };
    view.update_in(cx, |v, _window, cx| {
        let typed = [("$ sleep 9", prompt), ("", SemanticMark::Output)];
        v.term_event(session, marked_frame(1, &typed, 0), cx);
        v.term_event(session, marked_frame(2, &typed, 1), cx);
    });
    cx.run_until_parked();
    let closes = |sent: &[ClientMsg]| {
        sent.iter()
            .filter(|m| matches!(m, ClientMsg::Term { session: s, req: TermRequest::Close } if *s == session))
            .count()
    };
    fake.drain();
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert_eq!(closes(&fake.drain()), 0, "a running command: the shell asks first");
    assert!(cx.debug_bounds("close-confirm").is_some(), "the bar is up");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("close-confirm").is_none());
    cx.simulate_keystrokes("cmd-w");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Remove(i)) if *i == tile.item)),
        "↩ takes the tile off: {sent:?}"
    );
    assert_eq!(closes(&sent), 0, "the session waits for a change of mind");
    cx.executor().advance_clock(UNDO_CLOSE);
    cx.run_until_parked();
    assert_eq!(closes(&fake.drain()), 1, "then the worker closes it");
}

/// ⌘W on an idle shell takes its tile off but keeps the session for the undo window; ⌘Z puts
/// it back where it was, the same view, focused. After the window the session goes.
#[gpui::test]
fn a_closed_shell_can_be_taken_back(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let [_, (s2, second), _] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(second, cx));
    cx.run_until_parked();
    let first_view = view.read_with(cx, |v, _| v.terminal(s2).cloned().unwrap());
    fake.drain();
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Remove(i)) if *i == second.item)),
        "{sent:?}"
    );
    assert!(!view.read_with(cx, |v, _| v.layout().contains(second)), "off the strip");
    assert!(cx.debug_bounds("closed").is_some(), "the toast offers it back");

    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    let sent = fake.drain();
    assert!(
        sent.iter()
            .any(|m| matches!(m, ClientMsg::Items(ItemOp::Upsert(i)) if i.id == second.item)),
        "{sent:?}"
    );
    assert_eq!(column_of(&view, cx, second), 1, "back where it was");
    assert_eq!(focused(&view, cx), Some(second));
    assert!(
        view.read_with(cx, |v, _| v.terminal(s2).is_some_and(|t| *t == first_view)),
        "the same view"
    );
    assert!(terminal_focused(&view, cx, s2));

    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    fake.drain();
    cx.executor().advance_clock(UNDO_CLOSE);
    cx.run_until_parked();
    let sent = fake.drain();
    assert!(
        sent.iter().any(
            |m| matches!(m, ClientMsg::Term { session, req: TermRequest::Close } if *session == s2)
        ),
        "{sent:?}"
    );
}

// ----- the palette, names, notes -----------------------------------------------------------

/// ⌘⇧P lists the actions; typing narrows them; ↩ runs the one left once the palette is gone.
/// A session is a line too, and going to it focuses its tile.
#[gpui::test]
fn the_command_palette_runs_an_action_by_name(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_some(), "the palette is up");
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("New terminal ⌘T"))), "{tree:#?}");
    assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("Maximize column ⇧⌘↩"))), "{tree:#?}");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_none(), "Esc closes it");

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("n e w space n o t e");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let notes = view.read_with(cx, |v, _| {
        v.items().filter(|(_, i)| matches!(i.kind, ItemKind::Note { .. })).count()
    });
    assert_eq!(notes, 1, "the action ran");

    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, ClientId::new(), 1);
    assert_ne!(focused(&view, cx), Some(tile));
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("g o space t o space s h e l l enter");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(tile), "gone to");
    assert!(terminal_focused(&view, cx, session));
}

/// ⌘E names the focused tile: the field is in its header, ↩ sends the name to the worker.
#[gpui::test]
fn a_tile_is_named_from_its_header(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let tile = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    fake.drain();
    cx.simulate_keystrokes("cmd-e");
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("rename", tile.item)).is_some(), "the field is up");
    cx.simulate_keystrokes("a p i enter");
    cx.run_until_parked();
    let sent = fake.drain();
    assert!(
        sent.iter().any(
            |m| matches!(m, ClientMsg::Items(ItemOp::Upsert(i)) if i.name.as_deref() == Some("api"))
        ),
        "{sent:?}"
    );
    assert!(cx.debug_bounds(selector("rename", tile.item)).is_none(), "and it closed");
    let title = view.read_with(cx, |v, cx| v.card_title(tile, v.item(tile).unwrap(), cx));
    assert_eq!(title, "api");
}

/// A note opened here goes to the focused tile's worker, lands beside the focus, and holds
/// the keyboard for typing.
#[gpui::test]
fn a_new_note_opens_beside_the_focus_on_its_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    fake.drain();
    cx.simulate_keystrokes("cmd-shift-n");
    cx.run_until_parked();
    let sent = fake.drain();
    let note = sent
        .iter()
        .find_map(|m| match m {
            ClientMsg::Items(ItemOp::Upsert(i)) if matches!(i.kind, ItemKind::Note { .. }) => {
                Some(i.id)
            }
            _ => None,
        })
        .expect("the note went to the worker");
    let tile = TileRef { worker: fake.key, item: note };
    assert_eq!(focused(&view, cx), Some(tile));
    assert_eq!(column_of(&view, cx, tile), column_of(&view, cx, shell).saturating_add(1));
}

// ----- agents ------------------------------------------------------------------------------

/// An agent waiting on the human rings the tile in the warn tone, counts in the titlebar and
/// the Dock, and ⌘⇧A goes to it, across workers.
#[gpui::test]
fn an_agent_waiting_on_the_human_is_counted_and_reached(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let session = SessionId::new();
    let waiting = opens(&view, cx, &laptop, session, ClientId::new(), 1);
    let _mine = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let events = Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = Rc::clone(&events);
    cx.update(|_w, cx| {
        cx.subscribe(&view, move |_v, e: &WorkspaceEvent, _cx| seen.borrow_mut().push(*e)).detach();
    });
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(
            AgentEvent {
                session,
                kind: AgentKind::ClaudeCode,
                status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
                agent_session: None,
                detail: None,
                attention: true,
                source: AgentSource::Hook,
            },
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.needs_you_count()), 1);
    assert!(events.borrow().contains(&WorkspaceEvent::NeedsYou(1)), "{:?}", events.borrow());
    assert!(cx.debug_bounds("status-agents").is_some(), "the status bar says so");
    cx.simulate_keystrokes("cmd-shift-a");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(waiting));
    assert!(terminal_focused(&view, cx, session));
}

/// A worker's summaries name the agent in each session and where its status came from: a
/// connect shows its badge, counts it and knows whether the hooks are worth offering before any
/// agent event arrives, and a live event is not overwritten by a later summary.
#[gpui::test]
fn the_summaries_seed_the_agents_before_any_event(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (tx, _rx) = mpsc::channel(256);
    let key = WorkerKey::new(7);
    let (waiting, working) = (SessionId::new(), SessionId::new());
    let with = |session, status, source| SessionSummary {
        agent: Some(SessionAgent { kind: AgentKind::ClaudeCode, status, source }),
        ..summary(session, None)
    };
    let permission = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() });
    let sessions = vec![
        with(waiting, permission, AgentSource::Hook),
        with(working, AgentStatus::Working, AgentSource::Title),
        summary(SessionId::new(), None),
    ];
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    view.update_in(cx, |v, _window, cx| {
        v.add_worker(key, "studio".to_owned(), cx);
        let link = WorkerLink { me: ClientId::new(), out: tx, open_screen: factory, remote: None };
        v.connect_worker(key, "studio".to_owned(), link, sessions, cx);
        let tile = |session| Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            sleeping: false,
            name: None,
        };
        let items = vec![tile(waiting), tile(working)];
        v.apply_sync(key, ItemSync::Snapshot { version: 1, items }, cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert_eq!(v.needs_you_count(), 1, "the blocked agent counts at once");
        let status = |s| v.agent_state(s).map(|a| a.status.clone());
        assert_eq!(status(working), Some(AgentStatus::Working));
        let source = |s| v.agent_state(s).map(|a| a.source);
        assert_eq!(source(waiting), Some(AgentSource::Hook), "no hooks to offer there");
        assert_eq!(source(working), Some(AgentSource::Title), "the hooks would add to this");
        assert_eq!(v.agents.len(), 2, "the plain shell has no agent");
    });
    // The worker's first event says the agent moved on; a summary after it (the session
    // reopened in the list) does not take that back.
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Idle, ..blocked(waiting) }, cx);
        v.session_opened(key, with(waiting, AgentStatus::Working, AgentSource::Hook), cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert_eq!(v.agent_state(waiting).map(|a| a.status.clone()), Some(AgentStatus::Idle));
        assert_eq!(v.needs_you_count(), 0);
    });
}

fn blocked(session: SessionId) -> AgentEvent {
    AgentEvent {
        session,
        kind: AgentKind::ClaudeCode,
        status: AgentStatus::Blocked(BlockReason::Question),
        agent_session: None,
        detail: None,
        attention: true,
        source: AgentSource::Hook,
    }
}

/// An agent the server reports on a session with no tile here still counts, and ⌘⇧A gives it
/// a tile on its worker; on a worker this client cannot reach, ⌘⇧A says so instead.
#[gpui::test]
fn an_agent_the_server_reports_without_a_tile_is_counted_and_reached(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let laptop = WorkerKey::new(2);
    let (untiled, unreachable) = (SessionId::new(), SessionId::new());
    view.update_in(cx, |v, _w, cx| {
        v.add_worker(laptop, "laptop".into(), cx);
        v.server_agent_event(studio.key, blocked(untiled), cx);
    });
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| (v.needs_you_count(), v.needs_you_on(studio.key))),
        (1, 1)
    );
    assert!(cx.debug_bounds("status-agents").is_some(), "the status bar counts it");
    studio.drain();

    cx.simulate_keystrokes("cmd-shift-a");
    cx.run_until_parked();
    let asked = studio.drain();
    assert!(
        asked.iter().any(|m| matches!(
            m,
            ClientMsg::Items(ItemOp::Upsert(Item { kind: ItemKind::Terminal { session }, .. }))
                if *session == untiled
        )),
        "a tile for the waiting session: {asked:?}"
    );

    view.update_in(cx, |v, _w, cx| {
        v.server_session_closed(untiled, cx);
        v.server_agent_event(laptop, blocked(unreachable), cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.needs_you_on(laptop)), 1);
    cx.simulate_keystrokes("cmd-shift-a");
    cx.run_until_parked();
    let notice = view.read_with(cx, WorkspaceView::toast_text);
    assert_eq!(notice.as_deref(), Some("laptop is not reachable from here"));

    view.update_in(cx, |v, _w, cx| v.forget_server_agents(Some(laptop), cx));
    assert_eq!(view.read_with(cx, |v, _| v.needs_you_count()), 0, "a gone worker's agents go");
}

/// A worker the server says is away reads so in the titlebar, and the server's own line
/// shows only while it does not answer.
#[gpui::test]
fn the_servers_word_shows_quietly_in_the_titlebar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.update(|window, _cx| window.set_a11y_active(true));
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Unreachable, cx);
        v.set_server_status(Some("server unreachable".into()), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("server-status").is_some());
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let labels: Vec<&str> = tree.iter().filter_map(|n| n.label.as_deref()).collect();
    assert!(labels.contains(&"studio, unreachable"), "{labels:#?}");
    assert!(labels.contains(&"server unreachable"), "{labels:#?}");

    view.update_in(cx, |v, _w, cx| v.set_server_status(None, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("server-status").is_none(), "gone once the server answers");
}

/// "List workers" lists each worker with its state, and going to one without a tile asks it
/// for a shell.
#[gpui::test]
fn the_worker_list_goes_to_a_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    view.update_in(cx, |v, window, cx| v.list_workers(&ListWorkers, window, cx));
    cx.run_until_parked();
    let lines = view.read_with(cx, |v, cx| {
        v.palette.as_ref().map(|p| {
            p.read(cx)
                .matches(cx)
                .iter()
                .map(|l| (l.label.clone(), l.keys.clone()))
                .collect::<Vec<_>>()
        })
    });
    assert_eq!(lines, Some(vec![("Go to studio".to_owned(), String::new())]), "up: no word");
    studio.drain();
    view.update_in(cx, |v, _w, cx| v.go_to_worker(studio.key, cx));
    cx.run_until_parked();
    assert!(!studio.drain().is_empty(), "a worker with no tile is asked for a shell");
}

// ----- the layout on disk ------------------------------------------------------------------

/// The layout is written after it changes, and a new workspace read from the file puts every
/// tile back where it was, waiting for its worker.
#[gpui::test]
fn the_layout_is_saved_and_restored(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("slopty-layout-{}", ItemId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("layout.json");
    let (view, cx) = workspace(cx);
    view.update(cx, |v, _| v.set_layout_path(path.clone()));
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), _, (_, third)] = three_shells(&view, cx, &fake);
    cx.executor().advance_clock(SAVE_AFTER);
    cx.run_until_parked();
    let saved = read_layout(&path).expect("written");
    let restored = Layout::restore(saved, LayoutConfig::default());
    assert_eq!(restored.focused(), Some(third));
    assert_eq!(restored.position(first).map(|p| p.column), Some(0));
    assert!(read_layout(&dir.join("missing.json")).is_none(), "no file, no layout");
    std::fs::write(dir.join("bad.json"), "{").unwrap();
    assert!(read_layout(&dir.join("bad.json")).is_none(), "a bad file costs the layout only");
    std::fs::remove_dir_all(&dir).unwrap();
}

mod frame;
mod palette;
mod remote;
mod strip_marks;
mod tiles;

/// A worker that comes up with nothing on it is given a shell beside the rest, and the focus
/// stays where the human is typing: keys meant for one machine never land on another. The
/// first worker's shell, with nothing focused yet, takes the focus.
#[gpui::test]
fn a_new_workers_shell_opens_beside_without_taking_the_focus(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let typing = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    assert_eq!(focused(&view, cx), Some(typing), "the first shell takes the focus");
    let laptop = connect(&view, cx, 2, "laptop");
    let given = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 1);
    assert_eq!(focused(&view, cx), Some(typing), "the focus stays on the studio's shell");
    let workspace_of = |tile| {
        view.read_with(cx, |v, _| v.layout().position(tile).map(|p| p.workspace)).expect("placed")
    };
    assert_eq!(workspace_of(given), workspace_of(typing), "beside it, in the same workspace");
    let next = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 2);
    assert_eq!(focused(&view, cx), Some(next), "a shell the human opens takes the focus");
}

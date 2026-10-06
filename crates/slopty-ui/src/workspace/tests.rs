//! The workspace in a headless GPUI window: real layout, real key, mouse and scroll dispatch,
//! no process, no permissions, no pixels. Each worker is a channel: the test reads what the
//! workspace sends and feeds back what a worker would.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Entity, Modifiers, Pixels, Point, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
    VisualTestContext, point, px, size,
};
pub(super) use played::Agents;
use slopty_agent::status::{AgentEvent, AgentSource, AgentStatus, BlockReason};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::{ClientId, ItemId, SessionId, StreamId, WallMs};
use slopty_grid::{Cursor, Line, LineIndex, RowUpdate, SemanticMark, Style, TermModes};
use slopty_proto::ClientMsg;
use slopty_proto::agent::AgentKind;
use slopty_proto::handshake::HelloAck;
use slopty_proto::items::{Item, ItemKind, ItemOp, ItemSync};
use slopty_proto::screen::{CaptureTarget, ScreenEvent, ScreenRequest};
use slopty_proto::server::Os;
use slopty_proto::terminal::{
    BlockEnd, BlockMark, Blocks, Frame, OpenSession, SessionState, SessionSummary, TermEvent,
    TermRequest,
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

/// A workspace whose frames hold still: GPUI's fades and slides run on the wall clock, so a
/// test that judges a frame against one drawn from scratch ([`crate::retained::stale`]) runs
/// under Reduce Motion, where a slow machine cannot catch one half way.
fn still_workspace(cx: &mut TestAppContext) -> (Entity<WorkspaceView>, &mut VisualTestContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    workspace(cx)
}

/// Whether the last frame docked the navigator, read from the workspace rather than from debug
/// bounds: a measurement runs in release, where GPUI records none.
fn navigator_docked(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |v, _| v.nav.drawn == Some(navigator::Mode::Docked))
}

/// Where the last frame drew `tile`, from the area's own placements: in release too, where
/// GPUI records no debug bounds.
fn drawn_at(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
    tile: TileRef,
) -> Option<Bounds<Pixels>> {
    view.read_with(cx, |v, _| v.tile_bounds(tile))
}

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
            WorkerLink { me, out: tx, open_screen: factory, remote: None },
            hello(name, Vec::new()),
            cx,
        );
        v.apply_sync(key, ItemSync::Snapshot { version: 0, items: Vec::new() }, cx);
    });
    cx.run_until_parked();
    let mut fake = Fake { key, me, rx };
    fake.drain();
    fake
}

/// A Mac worker that says all is well: every grant, this build's version, Claude Code
/// installed.
fn healthy() -> WorkerCaps {
    WorkerCaps {
        os: Os::MacOs,
        os_version: "26.5".into(),
        can_capture: true,
        can_inject: true,
        version: env!("CARGO_PKG_VERSION").into(),
        agents: vec![slopty_proto::server::InstalledAgent {
            agent: slopty_proto::thread::AgentId::named(slopty_proto::thread::AgentId::CLAUDE_CODE),
            version: "2.1.0".into(),
            offers: slopty_proto::thread::Offers::default(),
        }],
        ..WorkerCaps::bare(Os::MacOs)
    }
}

/// The thread starts `fake` was sent, as (agent, folder, prompt).
fn thread_starts(fake: &mut Fake) -> Vec<(String, String, Option<String>)> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(slopty_proto::thread::wire::ThreadRequest::Start {
                start, ..
            }) => Some((start.agent.0, start.cwd, start.prompt)),
            _ => None,
        })
        .collect()
}

/// What a worker `name` says on a new link: no home, all well, `sessions` alive.
fn hello(name: &str, sessions: Vec<SessionSummary>) -> HelloAck {
    HelloAck {
        settings: String::new(),
        worker: slopty_core::WorkerId::new(),
        name: name.to_owned(),
        home: String::new(),
        caps: healthy(),
        load: 2.1,
        sessions,
    }
}

fn summary(session: SessionId, cwd: Option<&str>) -> SessionSummary {
    SessionSummary {
        id: session,
        title: "shell".into(),
        cwd: cwd.map(str::to_owned),
        repo: None,
        branch: None,
        changes: None,
        started_ms: WallMs::ZERO,
        cols: 80,
        rows: 24,
        state: SessionState::Running,
        viewers: 1,
        command: Vec::new(),
        progress: None,
        restored: None,
        repo_id: None,
    }
}

/// The keyboard handed from a shell to a file tile as the workspace draws reaches the shell's
/// drawing too: its view is drawn again without the keyboard (its caret a hollow block, not the
/// focused bar) in the frames that follow, though the move itself asks for no frame.
#[gpui::test]
fn a_shell_left_for_a_file_is_drawn_without_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let shell = opens_in(&view, cx, &studio, session, studio.me, 1, Some("/w"));
    let file = arrives(&view, cx, &studio, ItemKind::File { path: "/w/a.txt".to_owned() }, 2);
    // Both on show: the file in a pane right of the shell's.
    let pane = pos_of(&view, cx, shell).pane;
    view.update(cx, |v, cx| {
        let right = Some(slopty_client::layout::Side::Right);
        assert!(v.layout.place(file, slopty_client::layout::Drop { pane, edge: right }));
        cx.notify();
    });
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, session));
    let term = view.read_with(cx, |v, _| v.terminals.get(&session).cloned()).expect("a view");
    let drawn = |cx: &mut VisualTestContext| term.read_with(cx, |t, _| t.drawn_focused());
    assert_eq!(drawn(cx), Some(true), "drawn with the keyboard");
    view.update_in(cx, |v, _w, cx| v.focus_tile(file, cx));
    cx.run_until_parked();
    assert!(!terminal_focused(&view, cx, session), "the file has the keyboard");
    assert_eq!(drawn(cx), Some(false), "and the shell is drawn without it");
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
        name: None,
        facts: BTreeMap::new(),
    };
    let tile = TileRef { worker: fake.key, item: item.id };
    let key = fake.key;
    view.update_in(cx, |v, _window, cx| {
        v.session_opened(key, summary(session, cwd), cx);
        v.apply_sync(key, ItemSync::Delta { version, by, op: ItemOp::Add(item) }, cx);
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
    let item = Item { id: ItemId::new(), kind, name: None, facts: BTreeMap::new() };
    let tile = TileRef { worker: fake.key, item: item.id };
    let key = fake.key;
    view.update_in(cx, |v, _window, cx| {
        let by = ClientId::new();
        v.apply_sync(key, ItemSync::Delta { version, by, op: ItemOp::Add(item) }, cx);
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
        above: None,
        blocks: None,
        images: Vec::new(),
        updates: rows
            .iter()
            .enumerate()
            .map(|(row, text)| RowUpdate {
                row: u16::try_from(row).unwrap(),
                line: Line::from_text(text, 80, Style::DEFAULT).into(),
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
        above: None,
        blocks: Some(blocks_of(rows, cursor_row)),
        images: Vec::new(),
        updates: rows
            .iter()
            .enumerate()
            .map(|(row, (text, mark))| {
                let mut line = Line::from_text(text, 80, Style::DEFAULT);
                line.mark = *mark;
                RowUpdate { row: u16::try_from(row).unwrap(), line: line.into() }
            })
            .collect(),
    })
}

/// The worker's command blocks as `rows` and the cursor tell them, listed whole: a prompt's
/// block started once the cursor left its rows or a newer prompt followed, and ended with that
/// prompt's status once one did.
fn blocks_of(rows: &[(&str, SemanticMark)], cursor_row: u16) -> Blocks {
    let prompts: Vec<usize> = (0..rows.len()).filter(|&r| rows[r].1.starts_prompt()).collect();
    let mut marks = Vec::new();
    for (n, &prompt) in prompts.iter().enumerate() {
        let last_typed = (prompt..rows.len())
            .skip(1)
            .take_while(|&r| {
                matches!(rows[r].1, SemanticMark::Input | SemanticMark::PromptContinuation { .. })
            })
            .last()
            .unwrap_or(prompt);
        let end = prompts
            .get(n.saturating_add(1))
            .map(|&next| BlockEnd { exit: rows[next].1.exit(), took_ms: None });
        if end.is_some() || usize::from(cursor_row) > last_typed {
            marks.push(BlockMark { prompt: LineIndex(u64::try_from(prompt).unwrap()), end });
        }
    }
    Blocks { whole: true, marks }
}

/// `debug_bounds` wants a static selector; tests may leak a handful.
/// The accessibility tree of the next frame.
fn tree(cx: &mut VisualTestContext) -> Vec<crate::a11y::Node> {
    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.run_until_parked();
    cx.update(|window, _cx| crate::a11y::tree(window))
}

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

/// Where `tile` is: its project, its tab and its pane.
fn pos_of(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
    tile: TileRef,
) -> slopty_client::layout::Pos {
    view.read_with(cx, |v, _| v.layout().position(tile)).expect("placed")
}

/// `tiles` made the tabs of one pane, the first's, in their order.
fn one_pane(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tiles: &[TileRef]) {
    let Some((first, rest)) = tiles.split_first() else { return };
    let pane = pos_of(view, cx, *first).pane;
    view.update(cx, |v, cx| {
        for tile in rest {
            assert!(v.layout.place(*tile, slopty_client::layout::Drop { pane, edge: None }));
        }
        cx.notify();
    });
    cx.run_until_parked();
}

/// `tile`'s pane dragged to `width` by the tab's first upright sash, from whichever side of it
/// the pane stands, the room let down to a pane of 200 pt, and drawn again so a tab row's
/// scroll asked for lands.
fn pane_at(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tile: TileRef, width: f32) {
    for _ in 0..4 {
        let got =
            view.read_with(cx, |v, _| v.tile_bounds(tile)).map_or(0.0, |b| f32::from(b.size.width));
        if (got - width).abs() < 0.5 {
            break;
        }
        view.update_in(cx, |v, _w, cx| {
            v.layout.set_room(slopty_client::layout::Room { min_w: 200.0, min_h: 200.0 });
            let frame = v.layout.frame();
            let pane = v.layout.position(tile).expect("placed").pane;
            let x = frame.panes.iter().find(|l| l.pane == pane).map_or(0.0, |l| l.rect.x);
            let upright = |s: &&slopty_client::layout::tree::Sash| {
                matches!(s.axis, slopty_client::layout::tree::SplitAxis::Row)
            };
            let sash = frame.sashes.iter().find(upright).cloned().expect("a sash beside it");
            let delta = if x < sash.line.x { width - got } else { got - width };
            v.layout.drag_sash(&sash, delta);
            v.focus_tile(tile, cx);
            cx.notify();
        });
        cx.run_until_parked();
    }
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// `tile` moved into a pane of its own on `side` of `of`'s pane, on `of`'s tab: an arrival,
/// which comes in a background tab, brought beside what the test looks at with it.
fn beside(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    tile: TileRef,
    of: TileRef,
    side: slopty_client::layout::Side,
) {
    let pane = pos_of(view, cx, of).pane;
    view.update(cx, |v, cx| {
        let drop = slopty_client::layout::Drop { pane, edge: Some(side) };
        v.layout_action(cx, |l| assert!(l.place(tile, drop), "placed beside"));
    });
    cx.run_until_parked();
}

/// `tile` moved to a new tab of the project on show, and that tab shown.
fn on_new_tab(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tile: TileRef) {
    view.update(cx, |v, cx| {
        let home = v.layout.shown_project().map(|p| p.home().clone()).expect("a project");
        v.layout_action(cx, |l| {
            l.remove(tile);
            l.new_tab(tile, &home);
        });
    });
    cx.run_until_parked();
}

/// Three shells this client opened, one after the other, each beside the last by the room
/// rule: on the test's window the first above, the other two tabs of the pane below, the last
/// focused.
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
    let event = |phase, momentum_phase, dx: f32, dy: f32| ScrollWheelEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
        modifiers: Modifiers::default(),
        touch_phase: phase,
        momentum_phase,
    };
    cx.simulate_event(event(TouchPhase::Started, None, 0.0, 0.0));
    for _ in 0..steps {
        cx.simulate_event(event(TouchPhase::Moved, None, dx, dy));
    }
    cx.simulate_event(event(TouchPhase::Ended, None, 0.0, 0.0));
    for step in 0..momentum {
        let phase = match step {
            0 => TouchPhase::Started,
            _ if Some(step) == momentum.checked_sub(1) => TouchPhase::Ended,
            _ => TouchPhase::Moved,
        };
        cx.simulate_event(event(TouchPhase::Moved, Some(phase), dx, dy));
    }
    cx.run_until_parked();
}

// ----- the pure pieces ---------------------------------------------------------------------

/// A new note is named for the moment on this device's clock, in the directory given, else
/// the home; a directory's trailing slash and the root are kept whole.
#[test]
fn a_new_note_is_named_for_its_moment() {
    use super::commands::note_path;
    // 2024-02-29 12:34:56 UTC.
    let at = WallMs::from_millis(1_709_210_096_000);
    assert_eq!(note_path(None, at, 0).as_deref(), Some("~/note-2024-02-29-123456.md"));
    assert_eq!(
        note_path(Some("/w/app/"), at, 7 * 3_600).as_deref(),
        Some("/w/app/note-2024-02-29-193456.md")
    );
    assert_eq!(
        note_path(Some("/"), at, -13 * 3_600).as_deref(),
        Some("/note-2024-02-28-233456.md")
    );
}

/// A file's title is its name; the directory it is in is the header's context after it.
#[test]
fn a_file_tile_is_titled_by_its_name_and_placed_by_its_directory() {
    assert_eq!(file_title("/w/src/main.rs"), "main.rs");
    assert_eq!(tile::file_dir("/w/src/main.rs").as_deref(), Some("src"));
    assert_eq!(file_title("main.rs"), "main.rs");
    assert_eq!(tile::file_dir("main.rs"), None);
    assert_eq!(file_title("/etc/"), "etc");
    assert_eq!(tile::file_dir("/etc/"), None, "the root is no directory to name");
}

/// A file tile asks the worker for its text, takes the keyboard when focused, sends ⌘S as a
/// `WriteFile` to its own worker, marks the header while the edit is unsaved, and hears how
/// the write went.
#[gpui::test]
fn a_file_tile_is_edited_and_saved_through_its_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let path = "/w/notes.txt";
    let tile = arrives(&view, cx, &studio, ItemKind::File { path: path.to_owned() }, 1);
    assert!(
        studio.drain().iter().any(|m| matches!(m, ClientMsg::ReadFile { path: p } if p == path)),
        "the tile reads its file"
    );
    let text = slopty_proto::file::FileRead::Text {
        text: "# Notes".to_owned(),
        size: 8,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
        editorconfig: Vec::new(),
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
            base_modified_ms: Some(WallMs::from_millis(1_000)),
        }]
    );
    let saved =
        slopty_proto::file::WriteResult::Saved { size: 9, modified_ms: WallMs::from_millis(2_000) };
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

/// ⌘⇧T asks the focused tile's worker for a shell, and the echo of that item opens in a tab
/// of its own, focused, the terminal taking the keyboard. A second ⌘⇧T starts in the focused
/// shell's directory. ⌘D's shell splits off right of the focused pane and ⌘⇧D's below it,
/// each in the tab on show; a shell opened from a tile (a run's) goes beside by the room rule.
#[gpui::test]
fn new_shells_open_in_a_tab_or_split_off_the_focused_pane(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    assert!(cx.debug_bounds("workspace").is_some(), "the workspace is drawn");
    // The shell an empty worker is given comes first, as it was asked first.
    let given = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    fake.drain();

    cx.simulate_keystrokes("cmd-shift-t");
    let sent = fake.drain();
    assert!(
        matches!(
            sent.as_slice(),
            [ClientMsg::OpenSession { spec: OpenSession { cwd: None, .. }, .. }]
        ),
        "no shell to inherit from: {sent:?}"
    );
    let session = SessionId::new();
    let tile = opens_in(&view, cx, &fake, session, fake.me, 2, Some("/tmp/work"));
    assert_ne!(pos_of(&view, cx, tile).tab, pos_of(&view, cx, given).tab, "a tab of its own");
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

    cx.simulate_keystrokes("cmd-shift-t");
    let sent = fake.drain();
    assert!(
        matches!(
            sent.as_slice(),
            [ClientMsg::OpenSession { spec: OpenSession { cwd: Some(cwd), command, .. }, .. }]
                if cwd == "/tmp/work" && command.is_empty()
        ),
        "{sent:?}"
    );
    let tabbed = opens(&view, cx, &fake, SessionId::new(), fake.me, 3);
    assert_ne!(pos_of(&view, cx, tabbed).tab, pos_of(&view, cx, tile).tab, "a tab of its own");
    assert_eq!(focused(&view, cx), Some(tabbed));
    let tabs = view.read_with(cx, |v, _| v.layout().shown_project().map(|p| p.tabs().len()));
    assert_eq!(tabs, Some(3), "every tab in the one project");

    let rect = |cx: &mut VisualTestContext, t: TileRef| {
        view.read_with(cx, |v, _| {
            let pane = v.layout().position(t).expect("placed").pane;
            v.layout().frame().panes.iter().find(|l| l.pane == pane).map(|l| l.rect)
        })
        .expect("on show")
    };
    cx.simulate_keystrokes("cmd-d");
    let right = opens(&view, cx, &fake, SessionId::new(), fake.me, 4);
    assert_eq!(pos_of(&view, cx, right).tab, pos_of(&view, cx, tabbed).tab, "in the tab on show");
    let (from, to) = (rect(cx, tabbed), rect(cx, right));
    assert!(to.x >= from.right() - 0.5 && (to.y - from.y).abs() < 0.5, "{from:?} {to:?}");
    assert_eq!(focused(&view, cx), Some(right));

    cx.simulate_keystrokes("cmd-shift-d");
    let below = opens(&view, cx, &fake, SessionId::new(), fake.me, 5);
    let (from, to) = (rect(cx, right), rect(cx, below));
    assert!(to.y >= from.bottom() - 0.5 && (to.x - from.x).abs() < 0.5, "{from:?} {to:?}");
    assert_eq!(focused(&view, cx), Some(below));

    // A shell the worker makes for anything else (the self-test's) goes beside.
    view.update(cx, |v, cx| v.open_command(vec!["top".to_owned()], cx));
    let run = opens(&view, cx, &fake, SessionId::new(), fake.me, 6);
    assert_eq!(pos_of(&view, cx, run).tab, pos_of(&view, cx, below).tab, "beside, in its tab");
    let left = view.read_with(cx, |v, _| v.workers.get(&fake.key).map(|w| w.openings.len()));
    assert_eq!(left, Some(0), "nothing left waiting");
}

/// A shell that never comes takes its place in the queue with it: after a ⌘⇧T the worker
/// refused, ⌘D's shell still splits, rather than taking the refused one's tab.
#[gpui::test]
fn a_refused_shell_leaves_the_next_one_its_own_place(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    fake.drain();
    cx.simulate_keystrokes("cmd-shift-t");
    cx.simulate_keystrokes("cmd-d");
    let requests: Vec<u64> = fake
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::OpenSession { request, .. } => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 2, "{requests:?}");
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| v.open_failed(key, requests[0], "no such directory", cx));
    let split = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    let (a, b) = (pos_of(&view, cx, first), pos_of(&view, cx, split));
    assert_eq!(a.tab, b.tab, "split off in the tab on show, not a tab of its own");
    assert_ne!(a.pane, b.pane);
}

/// Keys typed into a program that stopped reading are refused on the worker, and the person
/// hears it as a notice rather than the log alone; so does an open the worker could not do,
/// named by where it was asked. Each open goes under a number of its own.
#[gpui::test]
fn a_refused_input_and_a_failed_open_are_notices(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let _tile = opens(&view, cx, &fake, session, fake.me, 1);
    let full = TermEvent::Error(slopty_proto::terminal::TermError::InputFull);
    view.update_in(cx, |v, _window, cx| v.term_event(session, full, cx));
    cx.run_until_parked();
    let notices = view.read_with(cx, |v, _| v.toast_texts());
    assert!(
        notices.iter().any(|n| n.starts_with("The program is not reading its input")),
        "{notices:?}"
    );

    cx.simulate_keystrokes("cmd-shift-t");
    cx.simulate_keystrokes("cmd-shift-t");
    let requests: Vec<u64> = fake
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::OpenSession { request, .. } => Some(request),
            _ => None,
        })
        .collect();
    assert!(requests.len() == 2 && requests[0] != requests[1], "{requests:?}");
    let key = fake.key;
    view.update_in(cx, |v, _window, cx| v.open_failed(key, requests[0], "no such directory", cx));
    cx.run_until_parked();
    let notices = view.read_with(cx, |v, _| v.toast_texts());
    assert!(
        notices.iter().any(|n| n == "Could not open a terminal on studio: no such directory"),
        "{notices:?}"
    );
}

/// What another client opens is a background tab at the end of its project, and the focus
/// stays where the human is.
#[gpui::test]
fn an_item_from_elsewhere_is_a_background_tab(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), (_, second), _] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    let theirs = opens(&view, cx, &fake, SessionId::new(), ClientId::new(), 4);
    assert_eq!(focused(&view, cx), Some(first), "the focus stayed");
    let (at, mine) = (pos_of(&view, cx, theirs), pos_of(&view, cx, second));
    assert_eq!(at.project, mine.project, "in the project of the worker's other work");
    assert_ne!(at.tab, mine.tab, "a tab of its own");
    let last = view.read_with(cx, |v, _| {
        v.layout().projects()[at.project].tabs().last().map(slopty_client::layout::Tab::id)
    });
    assert_eq!(last, Some(at.tab), "at the end");
    assert!(cx.debug_bounds(selector("item", theirs.item)).is_none(), "behind, not drawn");
}

/// Tiles of two workers share one tiling: a second worker's first tile from elsewhere goes to
/// its machine's project, and a tile knows which worker it is.
#[gpui::test]
fn two_workers_share_one_layout(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let a = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let b = opens(&view, cx, &laptop, SessionId::new(), ClientId::new(), 1);
    view.read_with(cx, |v, _| {
        let (pa, pb) = (v.layout().position(a).unwrap(), v.layout().position(b).unwrap());
        assert_ne!(pa.project, pb.project, "the laptop's first tile is its machine's project");
        assert_eq!(v.len(), 2);
        assert_eq!(b.worker, laptop.key);
    });
    // A tile the laptop's shell opens for this client lands beside the focus, whoever's.
    let c = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 2);
    assert_eq!(focused(&view, cx), Some(c));
    let (pa, pc) = view
        .read_with(cx, |v, _| (v.layout().position(a).unwrap(), v.layout().position(c).unwrap()));
    assert_eq!(pa.project, pc.project, "local opens go where the human is");
}

/// A worker that drops keeps its tiles, which say it is away; the navigator says so on its
/// header, and only its. When it comes back, a tile whose item is gone from its snapshot
/// leaves, the others stay.
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
    let labels = |cx: &mut VisualTestContext| {
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        tree.into_iter().filter_map(|n| n.label).collect::<Vec<_>>()
    };
    assert!(!labels(cx).iter().any(|l| l.ends_with(", reconnecting")), "nobody is down");

    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert!(v.layout().contains(kept) && v.layout().contains(gone), "the tiles stay");
    });
    let names = labels(cx);
    assert!(names.iter().any(|l| l == "studio, reconnecting"), "the one down says so: {names:#?}");
    assert!(names.iter().any(|l| l == "laptop"), "and not the one that is up: {names:#?}");

    // Back, with only `kept` in its registry.
    let (tx, _rx) = mpsc::channel(64);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    let item = view.read_with(cx, |v, _| v.item(kept).cloned()).unwrap();
    view.update_in(cx, |v, _w, cx| {
        let link = WorkerLink { me: studio.me, out: tx, open_screen: factory, remote: None };
        v.connect_worker(key, link, hello("studio", Vec::new()), cx);
        v.apply_sync(key, ItemSync::Snapshot { version: 9, items: vec![item] }, cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert!(v.layout().contains(kept), "still in the registry, still a tile");
        assert!(!v.layout().contains(gone), "gone from the registry, gone from the layout");
    });
}

// ----- the keyboard ------------------------------------------------------------------------

/// The pane keys move the focus and the tiles: ⌘⌥↑/↓ step between the panes, ⌘⌥[ through
/// a pane's tabs, ⌘⌥⇧↑ carries the tile into the pane above, and the keyboard follows the
/// focus into each terminal. ⇧⌘↩ zooms the pane over the tab, and again lets it go.
#[gpui::test]
fn the_pane_keys_move_the_focus_and_the_tiles(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(s1, first), (s2, second), (_s3, third)] = three_shells(&view, cx, &fake);
    assert_eq!(focused(&view, cx), Some(third));
    assert_eq!(pos_of(&view, cx, second).pane, pos_of(&view, cx, third).pane, "tabs of one pane");

    cx.simulate_keystrokes("cmd-alt-up");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(first));
    assert!(terminal_focused(&view, cx, s1), "the keyboard followed");

    cx.simulate_keystrokes("cmd-alt-down cmd-alt-[");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(second), "the pane's tab before");
    assert!(terminal_focused(&view, cx, s2));

    cx.simulate_keystrokes("cmd-alt-shift-up");
    cx.run_until_parked();
    assert_eq!(pos_of(&view, cx, second).pane, pos_of(&view, cx, first).pane, "carried up");
    assert_eq!(focused(&view, cx), Some(second), "and still focused");

    let width = |cx: &mut VisualTestContext| {
        cx.debug_bounds(selector("item", second.item)).map(|b| f32::from(b.size.width))
    };
    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
    let area = f32::from(cx.debug_bounds("area").expect("the area").size.width);
    let zoomed = view.read_with(cx, |v, _| v.layout().frame().panes.len());
    assert_eq!(zoomed, 1, "zoomed: one pane over the tab");
    assert!(width(cx).is_some_and(|w| (w - area).abs() < 2.0), "the whole width");
    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.layout().frame().panes.len()), 2, "and back");
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

/// A vertical swipe over a terminal is the terminal's: it scrolls into its history and the
/// pane holds still.
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
    assert_eq!(after.origin, before.origin, "the pane stayed");
}

/// Dragging a tile's header onto the middle of another pane makes it one of that pane's tabs.
#[gpui::test]
fn a_header_dragged_onto_a_pane_joins_its_tabs(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    // The third is the tab the pane below shows.
    let [(_, first), _, (_, third)] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    let header = cx.debug_bounds(selector("title", first.item)).unwrap().center();
    let onto = cx.debug_bounds(selector("item", third.item)).unwrap().center();
    cx.simulate_mouse_down(header, gpui::MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        point(header.x + px(20.0), header.y),
        Some(gpui::MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_move(onto, Some(gpui::MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(onto, gpui::MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    let (pf, pt) = (pos_of(&view, cx, first), pos_of(&view, cx, third));
    assert_eq!(pf.pane, pt.pane, "one pane now: {pf:?} {pt:?}");
    assert_eq!(focused(&view, cx), Some(first), "the dragged tile keeps the focus");
}

// ----- drawing only what shows -------------------------------------------------------------

/// Only what shows is drawn: a tile behind another in its pane has no element and its
/// terminal prepares no rows; shown, it is drawn.
#[gpui::test]
fn tiles_not_shown_are_not_drawn(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let mut tiles = Vec::new();
    for version in 1..=6 {
        tiles.push(opens(&view, cx, &fake, SessionId::new(), fake.me, version));
    }
    let drawn = view.read_with(cx, |v, _| v.drawn.placed.borrow().len());
    assert!(drawn < tiles.len(), "{drawn} of {} drawn", tiles.len());
    let hidden = tiles
        .iter()
        .copied()
        .find(|t| cx.debug_bounds(selector("item", t.item)).is_none())
        .expect("a tile behind another");
    let rows = cx.update(|_window, cx| crate::terminal::rows_prepared(cx));
    let per_grid = view.read_with(cx, |v, cx| {
        v.terminal(session_of(v, tiles[5])).and_then(|t| t.read(cx).metrics()).map(|m| m.rows)
    });
    assert!(
        per_grid.is_some_and(|r| rows <= usize::from(r).saturating_mul(drawn)),
        "only the drawn grids prepared rows: {rows}"
    );
    view.update_in(cx, |v, _w, cx| v.focus_tile(hidden, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("item", hidden.item)).is_some(), "shown: drawn");
}

/// A shell's output redraws that shell's tile only: its neighbours are replayed from the view
/// cache.
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
}

/// A remote window on a tab not shown lets its stream go after the grace, and asks for it
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
                stripes: Vec::new(),
            },
            cx,
        );
    });
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.screen(tile.item).is_some()), "streaming");
    assert_eq!(cx.active_idle_sleep_preventions(), 1, "a streaming window holds the device");

    // A shell of this client's on a tab of its own, shown over the window's.
    let shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    on_new_tab(&view, cx, shell);
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

/// Listing the installed fonts is a synchronous trip to the font server: the shells drawn at
/// once list them once, not once per view.
#[gpui::test]
fn the_installed_fonts_are_listed_once_for_every_shell(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    let drawn = shells
        .iter()
        .filter(|(_, tile)| cx.debug_bounds(selector("item", tile.item)).is_some())
        .count();
    assert!(drawn > 1, "shells drawn at once: {drawn}");
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
/// it back where it was, the same view, focused, but only while the notice is up. After the
/// window the session goes.
#[gpui::test]
fn a_closed_shell_can_be_taken_back(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let [_, (s2, second), _] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(second, cx));
    cx.run_until_parked();
    let first_view = view.read_with(cx, |v, _| v.terminal(s2).cloned().unwrap());
    fake.drain();
    let was = pos_of(&view, cx, second);
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Remove(i)) if *i == second.item)),
        "{sent:?}"
    );
    assert!(!view.read_with(cx, |v, _| v.layout().contains(second)), "out of its pane");
    assert!(cx.debug_bounds("closed").is_some(), "the toast offers it back");

    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Add(i)) if i.id == second.item)),
        "{sent:?}"
    );
    assert_eq!(pos_of(&view, cx, second).tab, was.tab, "back where it was");
    assert_eq!(focused(&view, cx), Some(second));
    assert!(
        view.read_with(cx, |v, _| v.terminal(s2).is_some_and(|t| *t == first_view)),
        "the same view"
    );
    assert!(terminal_focused(&view, cx, s2));

    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    fake.drain();
    let closed = |sent: &[ClientMsg]| {
        sent.iter().any(
            |m| matches!(m, ClientMsg::Term { session, req: TermRequest::Close } if *session == s2),
        )
    };
    cx.executor().advance_clock(UNDO_CLOSE);
    cx.run_until_parked();
    assert!(!closed(&fake.drain()), "an idle shell runs on past its notice");
    assert!(cx.debug_bounds("closed").is_none(), "the notice is gone");
    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.layout().contains(second)), "and ⌘Z with it");
    cx.executor().advance_clock(IDLE_SHELL_KEPT);
    cx.run_until_parked();
    assert!(closed(&fake.drain()), "then the worker closes it");
}

/// A closed tile stays on a list long after its notice: the palette's "Reopen" line brings it
/// back. A plain shell whose session has ended comes back as a new shell in its directory, and
/// only the last [`CLOSED_KEPT`] closings are kept, the oldest one's session ending as it
/// leaves the list.
#[gpui::test]
fn a_closed_tile_waits_in_the_palette_to_be_reopened(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens_in(&view, cx, &fake, session, fake.me, 1, Some("/w/src"));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    cx.executor().advance_clock(UNDO_CLOSE);
    cx.executor().advance_clock(IDLE_SHELL_KEPT);
    cx.run_until_parked();
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::Term { session: s, req: TermRequest::Close } if *s == session)),
        "the session has ended: {sent:?}"
    );
    let lines = view.read_with(cx, |v, _| v.closed_lines().len());
    assert_eq!(lines, 1, "the tile is still on the list");

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("r e o p e n enter");
    cx.run_until_parked();
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(
            m,
            ClientMsg::OpenSession { spec, .. }
                if spec.cwd.as_deref() == Some("/w/src") && spec.command.is_empty()
        )),
        "a new shell where it was: {sent:?}"
    );
    assert_eq!(view.read_with(cx, |v, _| v.closed_lines().len()), 0, "and off the list");

    let shells: Vec<SessionId> = (2..)
        .take(CLOSED_KEPT.saturating_add(1))
        .map(|version| {
            let session = SessionId::new();
            opens(&view, cx, &fake, session, fake.me, version);
            session
        })
        .collect();
    fake.drain();
    view.update_in(cx, |v, _w, cx| {
        for session in &shells {
            v.close_shell(*session, cx);
        }
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.closed_lines().len()), CLOSED_KEPT);
    let ended: Vec<SessionId> = fake
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Term { session, req: TermRequest::Close } => Some(session),
            _ => None,
        })
        .collect();
    assert_eq!(ended, shells[..1], "the oldest leaves the list and its session ends");
}

// ----- the palette, names, notes -----------------------------------------------------------

/// ⌘⇧P lists the actions; typing narrows them; Esc empties the field, and a second Esc closes
/// it; ↩ runs the one left once the palette is gone. A session is a line too, and going to it
/// focuses its tile.
#[gpui::test]
fn the_command_palette_runs_an_action_by_name(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_some(), "the palette is up");
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("New terminal ⇧⌘T"))), "{tree:#?}");
    // The empty field lists a few commands; typing reaches the rest.
    cx.simulate_keystrokes("p r e v i o u s space p r o");
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("Previous project ⌥⌘⇞"))), "{tree:#?}");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.palette_open()), "Esc empties the field first");
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("New terminal ⇧⌘T"))), "{tree:#?}");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.palette_open()), "then Esc closes it");

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("n e w space n o t e");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let notes = view.read_with(cx, |v, _| {
        let md = |p: &str| std::path::Path::new(p).extension().is_some_and(|e| e == "md");
        let note = |i: &Item| matches!(&i.kind, ItemKind::File { path } if md(path));
        v.items().filter(|(_, i)| note(i)).count()
    });
    assert_eq!(notes, 1, "the action ran");

    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, ClientId::new(), 1);
    assert_ne!(focused(&view, cx), Some(tile));
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("t e r m i n a l enter");
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
    let api = ClientMsg::Items(ItemOp::Rename { id: tile.item, name: Some("api".to_owned()) });
    assert_eq!(sent, vec![api], "the name alone");
    assert!(cx.debug_bounds(selector("rename", tile.item)).is_none(), "and it closed");
    let title = view.read_with(cx, |v, _| v.tile_title(v.item(tile).unwrap()));
    assert_eq!(title, "api");
}

/// A note opened here is a new Markdown file on the focused tile's worker, in its home while
/// the shell's directory is not known, named for the moment; it lands beside the focus with
/// its source holding the keyboard, and nothing is written until it is saved.
#[gpui::test]
fn a_new_note_opens_beside_the_focus_on_its_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    fake.drain();
    cx.simulate_keystrokes("cmd-shift-n");
    cx.run_until_parked();
    let sent = fake.drain();
    let (note, path) = sent
        .iter()
        .find_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(Item { id, kind: ItemKind::File { path }, .. })) => {
                Some((*id, path.clone()))
            }
            _ => None,
        })
        .expect("the note went to the worker");
    let md = std::path::Path::new(&path).extension().is_some_and(|e| e == "md");
    assert!(path.starts_with("~/note-") && md, "{path}");
    assert!(
        !sent.iter().any(|m| matches!(m, ClientMsg::WriteFile { .. })),
        "nothing is written yet"
    );
    let tile = TileRef { worker: fake.key, item: note };
    assert_eq!(focused(&view, cx), Some(tile));
    assert_eq!(pos_of(&view, cx, tile).tab, pos_of(&view, cx, shell).tab, "beside the shell");
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.file_read(
            key,
            &path,
            &slopty_proto::file::FileRead::Absent { editorconfig: Vec::new() },
            cx,
        );
    });
    cx.run_until_parked();
    let previewing =
        view.read_with(cx, |v, cx| v.files.get(&note).map(|f| f.read(cx).previewing()));
    assert_eq!(previewing, Some(false), "a new note is written in its source");
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
                since_ms: WallMs::ZERO,
                mode: None,
            },
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.needs_you_count()), 1);
    assert!(events.borrow().contains(&WorkspaceEvent::NeedsYou(1)), "{:?}", events.borrow());
    assert!(cx.debug_bounds("bell-count").is_some(), "the bell says so");
    cx.simulate_keystrokes("cmd-shift-a");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(waiting));
    assert!(view.read_with(cx, |v, _| v.face_shown(session)), "on its thread, where it asks");
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
        since_ms: WallMs::ZERO,
        mode: None,
    }
}

/// The worker `key`'s thread table naming a thread whose TUI runs in `session`, as the worker
/// says it of the agent at work there: the tile shows the thread once drawn again.
fn agent_thread(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    key: WorkerKey,
    session: SessionId,
) -> slopty_proto::thread::ThreadId {
    use slopty_proto::thread::Cursor;
    use slopty_proto::thread::wire::TableFrame;

    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let thread = state.meta.id;
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        v.thread_table(key, &table, cx);
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    thread
}

/// An agent on a session with no tile here still counts, and ⌘⇧A gives it a tile on its worker;
/// one the server reports on a worker this client cannot reach counts, and ⌘⇧A says so instead.
#[gpui::test]
fn an_agent_the_server_reports_without_a_tile_is_counted_and_reached(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let laptop = WorkerKey::new(2);
    let (untiled, unreachable) = (SessionId::new(), SessionId::new());
    view.update_in(cx, |v, _w, cx| {
        v.add_worker(laptop, "laptop".into(), cx);
        v.agent_event_on(studio.key, blocked(untiled), cx);
    });
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| (v.needs_you_count(), v.needs_you_on(studio.key))),
        (1, 1)
    );
    assert!(cx.debug_bounds("bell-count").is_some(), "the bell counts it");
    studio.drain();

    cx.simulate_keystrokes("cmd-shift-a");
    cx.run_until_parked();
    let asked = studio.drain();
    assert!(
        asked.iter().any(|m| matches!(
            m,
            ClientMsg::Items(ItemOp::Add(Item { kind: ItemKind::Terminal { session }, .. }))
                if *session == untiled
        )),
        "a tile for the waiting session: {asked:?}"
    );

    view.update_in(cx, |v, _w, cx| {
        v.agent_event_on(
            studio.key,
            AgentEvent { status: AgentStatus::None, ..blocked(untiled) },
            cx,
        );
        v.server_agent_event(laptop, blocked(unreachable), cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.needs_you_on(laptop)), 1);
    cx.simulate_keystrokes("cmd-shift-a");
    cx.run_until_parked();
    let notice = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(notice.as_deref(), Some("laptop is not reachable from here"));

    view.update_in(cx, |v, _w, cx| v.forget_server_agents(Some(laptop), cx));
    assert_eq!(view.read_with(cx, |v, _| v.needs_you_count()), 0, "a gone worker's agents go");
}

/// A worker the server says is away reads so in the navigator, and the server's own word leads
/// the title bar's readouts, in sentence case, only while it does not answer.
#[gpui::test]
fn the_servers_word_is_said_at_the_title_bars_end(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.update(|window, _cx| window.set_a11y_active(true));
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Unreachable, cx);
        v.set_server_status(Some("server offline".into()), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("readout-server").is_some());
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let labels: Vec<&str> = tree.iter().filter_map(|n| n.label.as_deref()).collect();
    assert!(labels.contains(&"studio, unreachable"), "{labels:#?}");
    assert!(labels.contains(&"Server offline"), "{labels:#?}");
    let (server, bar) = (
        cx.debug_bounds("readout-server").expect("drawn"),
        cx.debug_bounds("titlebar").expect("drawn"),
    );
    assert!(bar.contains(&server.center()), "in the title bar: {server:?} {bar:?}");
    let readouts = cx.debug_bounds("readouts").expect("drawn");
    assert!(server.left() - readouts.left() < px(1.0), "first among them: {server:?}");
    // Pressed, it offers what can be done, the app's rows, ending on its own right edge.
    let retried = Rc::new(std::cell::Cell::new(false));
    let seen = Rc::clone(&retried);
    view.update(cx, |v, _cx| {
        v.set_server_menu(vec![MenuEntry {
            group: MenuGroup::Connections,
            label: "Retry now".into(),
            detail: SharedString::default(),
            run: Rc::new(move |_window, _cx| seen.set(true)),
        }]);
    });
    cx.simulate_click(server.center(), Modifiers::none());
    cx.run_until_parked();
    let retry = cx.debug_bounds("menu-Retry now").expect("its menu");
    assert!(retry.right() <= server.right() + px(1.0), "under the readout: {retry:?}");
    cx.simulate_click(retry.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(retried.get(), "the row runs");

    view.update_in(cx, |v, _w, cx| v.set_server_status(None, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("readout-server").is_none(), "gone once the server answers");
}

/// The palette's line for each worker says its state, and going to one without a tile asks
/// it for a shell.
#[gpui::test]
fn the_worker_line_goes_to_a_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let lines =
        view.read_with(cx, |v, _| v.worker_lines().map(|l| (l.label, l.keys)).collect::<Vec<_>>());
    assert_eq!(lines, [("studio".to_owned(), String::new())], "up: no word");
    studio.drain();
    view.update_in(cx, |v, _w, cx| v.go_to_worker(studio.key, cx));
    cx.run_until_parked();
    assert!(!studio.drain().is_empty(), "a worker with no tile is asked for a shell");
}

// ----- the layout on disk ------------------------------------------------------------------

/// The layout is written after it changes, and a new workspace read from the file puts every
/// tile back where it was, waiting for its worker. A file that does not read is set aside as
/// `.bad` and said, once.
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
    let saved = read_layout(&path).ok().flatten().expect("written");
    let restored = Tiling::restore(saved.tiling, TilingConfig::default());
    assert_eq!(restored.focused(), Some(third));
    let path = |t: &Tiling, tile| {
        let at = t.position(tile)?;
        let tab = t.projects().get(at.project)?.tabs().iter().find(|x| x.id() == at.tab)?;
        tab.root().path_of(at.pane)
    };
    let was = view.read_with(cx, |v, _| path(v.layout(), first));
    assert_eq!(path(&restored, first), was, "in the pane it was in");
    assert_eq!(read_layout(&dir.join("missing.json")), Ok(None), "no file, no layout");
    std::fs::write(dir.join("bad.json"), "{").unwrap();
    assert_eq!(
        read_layout(&dir.join("bad.json")),
        Err(LayoutUnreadable),
        "a bad file costs the layout only"
    );
    assert!(!dir.join("bad.json").exists(), "set aside, so the next save keeps it");
    assert_eq!(std::fs::read_to_string(dir.join("bad.json.bad")).unwrap(), "{", "kept to read");
    assert_eq!(read_layout(&dir.join("bad.json")), Ok(None), "said once");
    std::fs::remove_dir_all(&dir).unwrap();
}

mod about;
mod address;
mod agent_tile;
mod attach_block;
mod away;
mod bars;
mod bodies;
mod context_menus;
mod cwd;
mod desktop;
mod faces;
mod facts;
mod focus;
mod focus_cache;
mod folders;
mod frame;
mod handoffs;
mod leaks;
mod measure;
mod menus;
mod modal_focus;
mod nav_list;
mod nav_projects;
mod nav_rows;
mod needs_you;
mod no_workers;
mod overlays;
mod page_chrome;
mod page_host;
mod palette;
mod palette_threads;
mod panes;
mod pins;
mod played;
mod popout;
mod presence;
mod projects;
mod relaunch;
mod remote;
mod retained;
mod review_tile;
mod rooms;
mod save_copy;
mod search;
mod shell_drag;
mod soak;
mod tab_commands;
mod tab_strip;
mod thread_face;
mod thread_start;
mod thread_waits;
mod tiles;
mod toasts;
mod touch;
mod unsaved;
mod worktrees;

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
    let project_of = |tile| pos_of(&view, cx, tile).project;
    assert_eq!(project_of(given), project_of(typing), "beside it, in the same project");
    let next = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 2);
    assert_eq!(focused(&view, cx), Some(next), "a shell the human opens takes the focus");
}

mod chrome;

/// The empty workspace sits where this frame's layout puts it, never where the area's size as
/// the last frame measured it would. When chrome comes or goes (the title bar's
/// buttons, with the first worker) the area changes size, and a page placed from the old size
/// stayed there until something else drew the area again: the app self-test's stale frame at
/// launch, its text 5 px low, a fifth of the 25 px bar.
#[gpui::test]
fn the_empty_workspace_is_placed_by_this_frames_layout(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let _studio = connect(&view, cx, 1, "studio");
    cx.run_until_parked();
    // One frame, drawn from scratch and nothing after it: what the window shows until the
    // next.
    let frame = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            crate::retained::painted(window)
        })
    };
    let settled = frame(cx);
    assert!(cx.debug_bounds("empty-worker-0").is_some(), "the empty workspace lists the worker");
    // What an area laid out taller in the last frame leaves for this one.
    view.update(cx, |v, _cx| {
        let mut was = v.drawn.viewport.get();
        was.size.height += px(25.0);
        v.drawn.viewport.set(was);
    });
    let first = frame(cx);
    let moved = first.iter().filter(|line| !settled.contains(line)).count();
    assert_eq!(moved, 0, "painted from the last frame's measure");
}

/// Secure keyboard entry holds while the focused shell reads a password, and lets go when
/// another tile has the focus, the program echoes again, or the person turns it off; set to
/// always, it holds while the window is in front.
#[gpui::test]
fn secure_entry_holds_while_the_focused_shell_reads_a_password(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.update(|window, _| window.activate_window());
    let fake = connect(&view, cx, 1, "studio");
    let [(_, left), (asking, right), _] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(right, cx));
    cx.run_until_parked();
    let on = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.secure_input_on());
    let prompt = |seq: u64, modes: TermModes| {
        let TermEvent::Frame(mut f) = frame(&["Password:", ""]) else { panic!("a frame") };
        f.seq = seq;
        f.modes = modes;
        TermEvent::Frame(f)
    };
    assert!(!on(cx));
    let password = TermModes::ECHO_OFF | TermModes::CANONICAL;
    view.update_in(cx, |v, _w, cx| v.term_event(asking, prompt(2, password), cx));
    cx.run_until_parked();
    assert!(on(cx), "the focused shell reads a password");
    view.update_in(cx, |v, _w, cx| v.focus_tile(left, cx));
    cx.run_until_parked();
    assert!(!on(cx), "another tile has the focus");
    view.update_in(cx, |v, _w, cx| v.focus_tile(right, cx));
    cx.run_until_parked();
    assert!(on(cx), "back on the prompt");
    // A line editor turns echo off too, but reads key by key: no password.
    view.update_in(cx, |v, _w, cx| v.term_event(asking, prompt(3, TermModes::ECHO_OFF), cx));
    cx.run_until_parked();
    assert!(!on(cx), "the shell's own prompt again");

    let with = |entry, cx: &mut VisualTestContext| {
        let mut theme = view.read_with(cx, |v, _| v.theme.clone());
        theme.behaviour.secure_entry = entry;
        view.update(cx, |v, cx| v.set_theme(theme, cx));
        cx.run_until_parked();
    };
    with(slopty_theme::SecureEntry::Always, cx);
    assert!(on(cx), "always, while the window is in front");
    cx.deactivate_window();
    cx.run_until_parked();
    assert!(!on(cx), "the app behind other windows");
    cx.update(|window, _| window.activate_window());
    with(slopty_theme::SecureEntry::Never, cx);
    view.update_in(cx, |v, _w, cx| v.term_event(asking, prompt(4, password), cx));
    cx.run_until_parked();
    assert!(!on(cx), "never, even at a password");
}

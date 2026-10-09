//! Nothing a tile draws lies past its edges, whatever room its pane gives it: each kind of
//! tile is laid out at the narrow, regular and wide widths a pane takes, and every node of
//! its accessibility tree is checked against the tile's own bounds (`docs/decisions/ui.md`,
//! "How surfaces adapt to their room").

use std::collections::HashMap;

use gpui::accesskit::{Node as AkNode, NodeId};
use slopty_proto::agent::{AgentBranch, Worktree};
use slopty_proto::folder::{FolderEntry, Listing};
use slopty_proto::orchestration::FileKind;
use slopty_proto::thread::Cursor;
use slopty_proto::thread::wire::TableFrame;

use super::*;

/// The widths a pane is checked at: a phone's narrowest, beside a board, a phone's, the narrow
/// edge, a half and the wide edge.
const WIDTHS: [f32; 6] = [280.0, 312.0, 360.0, 420.0, 560.0, 720.0];

/// A tile's height while it is checked.
const HIGH: f32 = 800.0;

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// `tile` on a tab of its own beside a shell in a pane on its right, so its header is its own
/// and not the title bar's and one sash sets its width, in a window wide enough for both at
/// any of [`WIDTHS`].
fn beside(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, fake: &Fake, tile: TileRef) {
    cx.simulate_resize(size(px(1600.0), px(HIGH)));
    let shell = opens(view, cx, fake, SessionId::new(), fake.me, 99);
    on_new_tab(view, cx, tile);
    super::beside(view, cx, shell, tile, slopty_client::layout::Side::Right);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    settle(cx);
}

/// `tile`'s pane dragged to `width` by the sash on its right, the least room let down to the
/// narrowest width checked.
fn at_width(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tile: TileRef, width: f32) {
    for _ in 0..4 {
        let got =
            view.read_with(cx, |v, _| v.tile_bounds(tile)).map_or(0.0, |b| f32::from(b.size.width));
        if (got - width).abs() < 0.5 {
            return;
        }
        view.update_in(cx, |v, _w, cx| {
            v.layout.set_room(slopty_client::layout::Room { min_w: WIDTHS[0], min_h: 200.0 });
            let sash = v.layout.frame().sashes.first().cloned().expect("a sash right of it");
            v.layout.drag_sash(&sash, width - got);
            v.focus_tile(tile, cx);
            cx.notify();
        });
        settle(cx);
    }
    panic!("the tile never came to {width} pt");
}

/// Every node under `tile`'s group that reaches past its left or right edge, by role and
/// label, with its bounds.
fn escaped(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tile: TileRef) -> Vec<String> {
    cx.update(|window, _| window.set_a11y_active(true));
    settle(cx);
    let at = view.read_with(cx, |v, _| v.tile_bounds(tile)).expect("the tile is drawn");
    let (left, right) = (f32::from(at.left()), f32::from(at.right()));
    cx.update(|window, _| {
        let Some(update) = window.a11y_tree() else { return vec!["no tree".to_owned()] };
        let nodes: HashMap<NodeId, &AkNode> = update.nodes.iter().map(|(id, n)| (*id, n)).collect();
        let scale = window.scale_factor().max(0.01);
        #[expect(clippy::cast_possible_truncation, reason = "window points")]
        let x =
            |node: &AkNode| node.bounds().map(|r| ((r.x0 as f32) / scale, (r.x1 as f32) / scale));
        let near = |a: f32, b: f32| (a - b).abs() < 1.0;
        // The outermost group the tile's bounds frame, found from the root down: its body's
        // groups have the same bounds.
        let mut order = vec![update.tree.as_ref().map(|t| t.root)];
        let mut group = None;
        while let Some(Some(id)) = order.pop() {
            let Some(node) = nodes.get(&id) else { continue };
            if format!("{:?}", node.role()) == "Group"
                && x(node).is_some_and(|(x0, x1)| near(x0, left) && near(x1, right))
            {
                group = Some((id, node));
                break;
            }
            order.extend(node.children().iter().rev().map(|c| Some(*c)));
        }
        let Some((root, _)) = group else { return vec!["no group for the tile".to_owned()] };
        let mut out = Vec::new();
        let mut seen = 0_usize;
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let Some(node) = nodes.get(&id) else { continue };
            let span = x(node).filter(|(x0, x1)| x1 > x0);
            seen = seen.saturating_add(usize::from(span.is_some()));
            if let Some((x0, x1)) = span
                && (x0 < left - 0.5 || x1 > right + 0.5)
            {
                out.push(format!(
                    "{:?} {:?} spans {x0:.1}..{x1:.1} in {left:.1}..{right:.1}",
                    node.role(),
                    node.label().unwrap_or_default(),
                ));
            }
            stack.extend(node.children().iter().copied());
        }
        // A tile says at least its header's name and a control: fewer means the tree is not
        // the tile's, and the check would pass on nothing.
        if seen < 3 {
            out.push(format!("only {seen} nodes under the tile"));
        }
        out
    })
}

/// `tile`, beside another pane, at each of [`WIDTHS`]: nothing escapes it.
fn contained(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    tile: TileRef,
    kind: &str,
) {
    beside(view, cx, fake, tile);
    let mut wrong = Vec::new();
    for width in WIDTHS {
        at_width(view, cx, tile, width);
        wrong
            .extend(escaped(view, cx, tile).into_iter().map(|e| format!("{kind} at {width}: {e}")));
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// An agent at work in a shell, with its pull request and its worktree on the header.
fn agent_with_chips(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
) -> (TileRef, SessionId) {
    let session = SessionId::new();
    let tile = opens(view, cx, fake, session, fake.me, 1);
    let branch = AgentBranch {
        session,
        worktree: Some(Worktree {
            name: "responsive-tile-headers".to_owned(),
            path: "/r/.claude/worktrees/responsive-tile-headers".to_owned(),
            branch: Some("worktree-responsive-tile-headers".to_owned()),
            original_cwd: "/r".to_owned(),
            original_branch: Some("main".to_owned()),
        }),
    };
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.agent_branch(branch, cx);
    });
    settle(cx);
    (tile, session)
}

/// A shell, an agent's shell with its chips, the agent's thread, a board, a file, a folder, a
/// folder's changes in review, a page, a remote window waiting and one that did not open, and a
/// pane of three tabs: at every width a pane takes, nothing any of them draws lies past the
/// tile's edges. A clipped send button or a chip run under the next pane fails here.
#[gpui::test]
fn nothing_escapes_its_tile_at_any_room(app: &mut TestAppContext) {
    let (view, cx) = still_workspace(app);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    contained(&view, cx, &studio, shell, "a shell");

    let (view, cx) = still_workspace(app);
    let studio = connect(&view, cx, 1, "studio");
    let (agent, _) = agent_with_chips(&view, cx, &studio);
    contained(&view, cx, &studio, agent, "an agent's shell");

    let (view, cx) = still_workspace(app);
    let studio = connect(&view, cx, 1, "studio");
    let (agent, session) = agent_with_chips(&view, cx, &studio);
    let key = studio.key;
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        v.thread_table(key, &table, cx);
    });
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.face_shown(session)), "on its thread");
    contained(&view, cx, &studio, agent, "an agent's thread");

    let (view, cx) = still_workspace(app);
    let (studio, orchestrator, session) = projects::orchestrator(&view, cx);
    view.update_in(cx, |v, _w, cx| {
        v.focus_tile(orchestrator, cx);
        v.show_face(session, false, cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-j");
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.board_shown(session)), "on its board");
    contained(&view, cx, &studio, orchestrator, "a board");

    let (view, cx) = still_workspace(app);
    let studio = connect(&view, cx, 1, "studio");
    let path = "/w/a-file-whose-name-runs-on-past-any-narrow-pane.md";
    let file = arrives(&view, cx, &studio, ItemKind::File { path: path.to_owned() }, 1);
    let text = slopty_proto::file::FileRead::Text {
        text: "# Notes\n\nA line long enough to wrap in a narrow pane, and then some more.\n"
            .to_owned(),
        size: 80,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
        editorconfig: Vec::new(),
    };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.file_read(key, path, &text, cx));
    settle(cx);
    contained(&view, cx, &studio, file, "a file");

    let (view, cx) = still_workspace(app);
    let studio = connect(&view, cx, 1, "studio");
    let folder = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/proj".into() }, 1);
    let entry = |name: &str, kind: FileKind| FolderEntry {
        name: name.to_owned(),
        kind,
        link: false,
        hidden: false,
        size: 1_234,
        items: (kind == FileKind::Dir).then_some(2),
        modified_ms: WallMs::from_millis(1_700_000_000_000),
    };
    let listing = Listing::Listed {
        dir: "/w/proj".to_owned(),
        entries: vec![
            entry("src", FileKind::Dir),
            entry("a-file-whose-name-runs-on-past-any-narrow-pane.rs", FileKind::File),
        ],
        total: 2,
    };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.folder_listed(key, "/w/proj", &listing, cx));
    settle(cx);
    contained(&view, cx, &studio, folder, "a folder");

    let (view, cx) = still_workspace(app);
    let studio = connect(&view, cx, 1, "studio");
    let review = changes_in_review(&view, cx, studio);
    contained(&view, cx, &review.0, review.1, "a folder's changes in review");

    let (view, cx) = still_workspace(app);
    let studio = connect(&view, cx, 1, "studio");
    cx.update(|window, _| window.activate_window());
    let url = "http://127.0.0.1:5173/a/path/long/enough/to/run/past/any/narrow/pane".to_owned();
    let page = arrives(&view, cx, &studio, ItemKind::Browser { url }, 1);
    view.update_in(cx, |v, window, cx| {
        v.focus_tile(page, cx);
        let view = v.browser(page.item).cloned().expect("a page view");
        view.update(cx, |page, cx| page.open_stand_in(window, cx));
    });
    settle(cx);
    contained(&view, cx, &studio, page, "a page");

    let (view, cx) = still_workspace(app);
    let studio = connect(&view, cx, 1, "studio");
    let window = slopty_core::WindowId(7);
    let remote = arrives(&view, cx, &studio, ItemKind::Window { window }, 1);
    cx.executor().advance_clock(crate::screen::LOADING_GRACE);
    settle(cx);
    assert!(cx.debug_bounds(selector("waiting", remote.item)).is_some(), "it says it is opening");
    contained(&view, cx, &studio, remote, "a remote window opening");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        use slopty_proto::screen::{CaptureTarget, OpenAsk, ScreenEvent, ScreenFailure};
        let asked = OpenAsk::Target(CaptureTarget::Window(window));
        v.screen_event(key, ScreenEvent::OpenFailed { asked, why: ScreenFailure::Gone }, cx);
    });
    cx.executor().advance_clock(crate::screen::LOADING_GRACE);
    settle(cx);
    contained(&view, cx, &studio, remote, "a remote window that did not open");

    let (view, cx) = still_workspace(app);
    let studio = connect(&view, cx, 1, "studio");
    let tabs: Vec<TileRef> = (1..=3)
        .map(|version| opens(&view, cx, &studio, SessionId::new(), studio.me, version))
        .collect();
    view.update_in(cx, |v, _w, cx| {
        let pane = v.layout.position(tabs[0]).expect("placed").pane;
        for tab in tabs.iter().skip(1) {
            v.layout.place(*tab, slopty_client::layout::Drop { pane, edge: None });
        }
        cx.notify();
    });
    settle(cx);
    let last = *tabs.last().expect("three tabs");
    contained(&view, cx, &studio, last, "a pane of three tabs");
}

/// A folder's changes opened as a tile of their own, the worker's answer in: a file whose path
/// runs on past a narrow pane, with the scope bar and the file's head. The worker, then the
/// tile.
fn changes_in_review(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    mut studio: Fake,
) -> (Fake, TileRef) {
    use slopty_proto::git::{GitDone, GitOp, GitOutcome};
    use slopty_proto::thread::Patch;
    use slopty_proto::thread::wire::{Against, FileDiff, Review, ReviewScope};

    use super::super::actions::ReviewChanges;

    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let folder = arrives(view, cx, &studio, ItemKind::Folder { path: "/w/atlas".into() }, 1);
    view.update_in(cx, |v, _w, cx| v.focus_tile(folder, cx));
    cx.run_until_parked();
    studio.drain();
    cx.dispatch_action(ReviewChanges);
    settle(cx);
    let sent = studio.drain();
    let tile = sent.iter().find_map(|m| match m {
        ClientMsg::Items(ItemOp::Add(item)) => Some(item.id),
        _ => None,
    });
    let request = sent.iter().find_map(|m| match m {
        ClientMsg::Git { request, op: GitOp::Changes { .. }, .. } => Some(*request),
        _ => None,
    });
    let (Some(item), Some(request)) = (tile, request) else { panic!("a review asked: {sent:?}") };
    let file = FileDiff {
        path: "crates/a-crate-whose-name-runs-on/src/a-file-whose-name-runs-on-past-any-pane.rs"
            .to_owned(),
        from: Some("old".to_owned()),
        to: Some("new".to_owned()),
        kind: slopty_proto::thread::wire::FileKind::Text,
        old_path: None,
        modes: None,
        patch: Patch { hunks: Vec::new(), added: 12, removed: 3, clipped_lines: 0, full: None },
    };
    let review = Review {
        scope: ReviewScope::WorkingTree(Against::Head),
        from: None,
        to: None,
        files: vec![file],
        absent: None,
    };
    let done = GitOutcome::Done(GitDone::Changes(Box::new(review)));
    view.update_in(cx, |v, _w, cx| v.git_done(key, request, done, cx));
    settle(cx);
    assert!(cx.debug_bounds("review-head-0").is_some(), "the file shows");
    let tile = TileRef { worker: key, item };
    assert!(view.read_with(cx, |v, _| v.tile_bounds(tile)).is_some(), "a tile of its own");
    (studio, tile)
}

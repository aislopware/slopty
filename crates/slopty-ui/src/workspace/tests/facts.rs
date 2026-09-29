//! What the chrome says of a tile and of a worker: a shell's title and its place, a command
//! running, a repository's changes, a worker's health and its machine, the status bar's facts,
//! and where a new tile goes.

use std::time::Duration;

use slopty_proto::server::{Os, WorkerCaps};
use slopty_proto::terminal::RepoChanges;

use super::*;
use crate::icons::Status;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

/// A worker `name` connects saying its home is `home` and what it is.
fn connect_as(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    seed: u128,
    name: &str,
    home: &str,
    caps: WorkerCaps,
) -> Fake {
    let (tx, rx) = mpsc::channel(256);
    let me = ClientId::new();
    let key = WorkerKey::new(seed);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    view.update_in(cx, |v, _window, cx| {
        v.add_worker(key, name.to_owned(), cx);
        let ack = HelloAck { home: home.to_owned(), caps, ..hello(name, Vec::new()) };
        v.connect_worker(
            key,
            WorkerLink { me, out: tx, open_screen: factory, remote: None },
            ack,
            cx,
        );
        v.apply_sync(key, ItemSync::Snapshot { version: 0, items: Vec::new() }, cx);
    });
    cx.run_until_parked();
    let mut fake = Fake { key, me, rx };
    fake.drain();
    fake
}

/// `command` typed at a prompt in `session` and entered: the shell now runs it.
fn run(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    session: SessionId,
    command: &str,
) {
    let prompt = SemanticMark::Prompt { exit: None, input: Some(2) };
    let typed = format!("$ {command}");
    let rows = [(typed.as_str(), prompt), ("", SemanticMark::Output)];
    view.update_in(cx, |v, _w, cx| {
        v.term_event(session, marked_frame(1, &rows, 0), cx);
        v.term_event(session, marked_frame(2, &rows, 1), cx);
    });
    cx.run_until_parked();
}

/// A shell is called by the command it runs, else a title its program set, else the
/// repository or directory it stands in, else "Terminal"; an agent's, by the agent. After the
/// title, the header says where it is less what the title already said. A home the worker
/// named is written `~` wherever it is.
#[gpui::test]
fn a_shell_is_titled_by_what_it_runs_then_its_own_title_then_where_it_is(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect_as(&view, cx, 1, "studio", "/Volumes/Data/me", healthy());
    let session = SessionId::new();
    let tile = opens_in(&view, cx, &fake, session, fake.me, 1, Some("/Volumes/Data/me"));
    let said = |view: &Entity<WorkspaceView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| {
            let place = v.item(tile).and_then(|item| v.tile_place(item, cx));
            (v.terminal_title(session, cx), place)
        })
    };
    assert_eq!(said(&view, cx), ("Terminal".into(), Some("~".into())), "home, known exactly");

    let repo = "/Volumes/Data/me/oss/slopty";
    // As the shell's report of it does: the workspace changed, and the next frame says so.
    let moved = |view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, cwd: &str| {
        view.update_in(cx, |v, _w, cx| {
            v.session_moved(session, cwd, Some(repo), Some("main"));
            cx.notify();
        });
        cx.run_until_parked();
    };
    moved(&view, cx, repo);
    assert_eq!(said(&view, cx), ("slopty".into(), Some("main".into())), "named by its repo");
    moved(&view, cx, &format!("{repo}/crates/ui"));
    assert_eq!(said(&view, cx), ("slopty".into(), Some("crates/ui".into())));

    // The shell's own name set as a title says nothing; a program's title does.
    let title = |view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, t: &str| {
        view.update_in(cx, |v, _w, cx| v.term_event(session, TermEvent::Title(t.into()), cx));
        cx.run_until_parked();
    };
    title(&view, cx, "zsh");
    assert_eq!(said(&view, cx).0, "slopty");
    title(&view, cx, "me@studio:~/oss/slopty");
    assert_eq!(said(&view, cx).0, "slopty", "a prompt's title is the place, said better");
    title(&view, cx, "htop");
    assert_eq!(said(&view, cx), ("htop".into(), Some("slopty/crates/ui".into())));

    run(&view, cx, session, "cargo test -p slopty-ui");
    assert_eq!(said(&view, cx).0, "cargo test -p slopty-ui", "what it runs, first");

    title(&view, cx, "claude");
    view.update_in(cx, |v, _w, cx| {
        let working = AgentEvent { status: AgentStatus::Working, ..blocked(session) };
        v.agent_event(working, cx);
    });
    cx.run_until_parked();
    let agent = view.read_with(cx, |v, cx| v.terminal_title(session, cx));
    assert_eq!(agent, "claude", "a title its program set");
    title(&view, cx, "/Volumes/Data/me");
    let agent = view.read_with(cx, |v, cx| v.terminal_title(session, cx));
    assert_eq!(agent, "Claude Code", "else the agent's name");

    let lines = view.read_with(cx, WorkspaceView::navigator_lines);
    assert!(lines.iter().any(|(t, ..)| t == "Claude Code"), "{lines:#?}");
}

/// A command past the threshold is Running: the calm mark in the header with how long it has
/// run, the time at the end of its navigator row, the status bar's fact, and the least of what
/// a worker's rollup counts.
#[gpui::test]
fn a_long_command_shows_running_and_its_time_everywhere(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, fake.me, 1);
    view.update_in(cx, |v, _w, _cx| v.running_after = Duration::ZERO);
    run(&view, cx, session, "sleep 60");
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let mark = view.read_with(cx, |v, cx| {
        let item = v.item(tile).cloned();
        item.and_then(|item| v.tile_status(tile, &item, cx))
    });
    assert_eq!(mark, Some(Status::Running));
    let id = tile.item.as_uuid();
    assert!(cx.debug_bounds(leak(format!("running-{id}"))).is_some(), "the header's time");
    assert!(cx.debug_bounds(leak(format!("nav-running-{id}"))).is_some(), "the row's time");
    assert!(cx.debug_bounds("status-facts").is_some(), "the status bar's");
    let rollup = view.read_with(cx, |v, cx| v.worker_rollup(fake.key, cx));
    assert_eq!(rollup.shown(), Some(rollup::Shown::Running));

    // Under the threshold, nothing: a quick command ends before a mark would be read.
    view.update_in(cx, |v, _w, cx| {
        v.running_after = RUNNING_AFTER;
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds(leak(format!("running-{id}"))).is_none());
}

/// What a repository's working tree changed shows at the end of the shell's navigator row and
/// after the branch in the status bar, as `kit::changes` draws a diff's size.
#[gpui::test]
fn repo_changes_show_in_the_row_and_the_bar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, fake.me, 1);
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        let summary = SessionSummary {
            repo: Some("/Users/me/oss/slopty".into()),
            branch: Some("main".into()),
            changes: Some(RepoChanges { files: 2, added: 12, removed: 3 }),
            ..summary(session, Some("/Users/me/oss/slopty"))
        };
        v.session_opened(key, summary, cx);
    });
    cx.run_until_parked();
    let id = tile.item.as_uuid();
    let row = cx.debug_bounds(leak(format!("nav-changes-{id}"))).expect("the row's changes");
    let meta = cx.debug_bounds(leak(format!("nav-meta-{id}"))).expect("the second line");
    assert!(row.left() >= meta.right(), "after the words: {row:?} {meta:?}");
    let bar = cx.debug_bounds("status-changes").expect("the bar's changes");
    let branch = cx.debug_bounds("status-branch").expect("the branch");
    assert!(bar.left() >= branch.right(), "after the branch");
    let lines = navigator::line_changes(RepoChanges { files: 2, added: 12, removed: 3 });
    assert_eq!(lines, Some((12, 3)), "drawn by `kit::changes`");
    assert_eq!(navigator::line_changes(RepoChanges::default()), None, "clean");
    let renamed = RepoChanges { files: 1, added: 0, removed: 0 };
    assert_eq!(navigator::line_changes(renamed), None, "no line changed, no figure");
}

/// A worker's header says what is wrong with it only when something is: Screen Recording or
/// Accessibility off on a Mac, another version. The hosts list reads its machine, and a window
/// asked of a worker that cannot capture says why instead of an empty picker.
#[gpui::test]
fn a_workers_health_shows_only_when_something_is_wrong(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect_as(&view, cx, 1, "studio", "/Users/me", healthy());
    let key = fake.key;
    let warn = leak(format!("nav-worker-warn-{key}"));
    assert!(cx.debug_bounds(warn).is_none(), "all well, nothing said");
    let blind = WorkerCaps { can_capture: false, ..healthy() };
    view.update_in(cx, |v, _w, cx| v.set_worker_caps(key, blind, cx));
    cx.run_until_parked();
    let line = cx.debug_bounds(warn).expect("a warn line");
    let name = cx.debug_bounds(leak(format!("nav-worker-name-{key}"))).expect("the name");
    assert!(line.top() >= name.bottom() - px(0.5), "under the name");
    let caps = view.read_with(cx, |v, _| v.workers.get(&key).and_then(|w| w.caps.clone()));
    let said = caps.as_ref().and_then(navigator::worker_warning);
    assert_eq!(said.as_deref(), Some("Screen Recording off"));
    let other = WorkerCaps { version: "0.0.1".into(), can_inject: false, ..healthy() };
    assert_eq!(
        navigator::worker_warning(&other).as_deref(),
        Some("Accessibility off"),
        "a version is the wire fingerprint's to judge, not the header's"
    );
    let linux = WorkerCaps { os: Os::Linux, can_capture: false, can_inject: false, ..healthy() };
    assert_eq!(navigator::worker_warning(&linux), None, "not a Mac's grants");

    view.update_in(cx, |v, window, cx| v.add_window(&AddWindow, window, cx));
    cx.run_until_parked();
    let notices = view.read_with(cx, WorkspaceView::toast_texts);
    assert!(notices.iter().any(|n| n.contains("Screen Recording is off")), "{notices:?}");
    // A Linux worker has no capture at all: ⌘O says so, not that a Mac's grant is off.
    view.update_in(cx, |v, _w, cx| v.set_worker_caps(key, linux, cx));
    view.update_in(cx, |v, window, cx| v.add_window(&AddWindow, window, cx));
    cx.run_until_parked();
    let notices = view.read_with(cx, WorkspaceView::toast_texts);
    assert!(notices.iter().any(|n| n.contains("it has no screen capture")), "{notices:?}");

    view.update_in(cx, |v, _w, cx| v.toggle_hosts(cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(leak(format!("hosts-machine-{key}"))).is_some(), "its machine");
    assert_eq!(navigator::host_line(&healthy(), Some(2.1)), "macOS 26.5 \u{b7} load 2.1");
    assert_eq!(navigator::host_line(&healthy(), None), "macOS 26.5", "no load heard yet");
}

/// The status bar is never empty: with one worker and nothing that says where, it names the
/// worker; a focused page leaves its host to its header, so the bar names the worker there too.
#[gpui::test]
fn the_status_bar_always_says_something(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    arrives(&view, cx, &fake, ItemKind::Note { text: "plan".into() }, 1);
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-worker").is_some(), "a note says nowhere: the worker");
    let page =
        arrives(&view, cx, &fake, ItemKind::Browser { url: "http://localhost:5173/app".into() }, 2);
    view.update_in(cx, |v, _w, cx| v.focus_tile(page, cx));
    cx.run_until_parked();
    let facts = view.read_with(cx, |v, cx| {
        let item = v.item(page).cloned();
        item.and_then(|item| v.focus_facts(&item, cx))
    });
    assert_eq!(facts, None, "the host is the header's place, said once");
    assert!(cx.debug_bounds("status-worker").is_some(), "a page says nowhere else: the worker");
}

/// With several workers the empty workspace's rows each open a shell on theirs, here; and "+"
/// chooses the worker a new tile goes to, keeping its menu open on the kinds of tile.
#[gpui::test]
fn a_new_tile_can_go_to_any_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let mut laptop = connect(&view, cx, 2, "laptop");
    let opened = |fake: &mut Fake| {
        fake.drain().into_iter().any(|m| matches!(m, ClientMsg::OpenSession { .. }))
    };
    let order: Vec<WorkerKey> = view.read_with(cx, |v, _| v.workers.keys().copied().collect());
    let laptop_row = order.iter().position(|k| *k == laptop.key).expect("listed");
    click(cx, leak(format!("empty-worker-{laptop_row}")));
    assert!(opened(&mut laptop), "a shell on the row's worker");
    assert!(!opened(&mut studio));

    view.update_in(cx, |v, _w, cx| {
        v.menu = Some(titlebar::MenuKind::New);
        cx.notify();
    });
    cx.run_until_parked();
    click(cx, "menu-laptop");
    assert_eq!(view.read_with(cx, |v, _| v.menu), Some(titlebar::MenuKind::New), "still open");
    click(cx, "menu-New terminal");
    assert!(opened(&mut laptop), "the chosen worker");
    assert!(!opened(&mut studio));
}

/// The palette lists tiles from the latest used, agents waiting on the human first and the
/// focused tile last: nobody goes where they are.
#[gpui::test]
fn the_palette_lists_tiles_by_recency_with_the_focused_last(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(a, ta), (b, tb), (c, tc)] = three_shells(&view, cx, &fake);
    for tile in [tb, ta, tc] {
        view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    }
    cx.run_until_parked();
    let sessions = |view: &Entity<WorkspaceView>, cx: &mut VisualTestContext| {
        view.update_in(cx, |v, _w, cx| {
            v.session_rows(cx).into_iter().map(|r| r.session).collect::<Vec<_>>()
        })
    };
    assert_eq!(sessions(&view, cx), vec![a, b, c], "the latest first, the focused last");
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(b), cx));
    cx.run_until_parked();
    assert_eq!(sessions(&view, cx), vec![b, a, c], "who waits on the human first");
}

/// An overview miniature's label says more than a title: the tile's state, one muted line of
/// where it is, and its worker where there are several; a workspace's name carries its rollup.
#[gpui::test]
fn overview_labels_say_state_place_and_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let _laptop = connect(&view, cx, 2, "laptop");
    let shells = three_shells(&view, cx, &fake);
    let key = fake.key;
    for (session, _) in &shells {
        let moved = summary(*session, Some("/Users/me/oss/slopty"));
        view.update_in(cx, |v, _w, cx| v.session_opened(key, moved, cx));
    }
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(shells[0].0), cx));
    run(&view, cx, shells[2].0, "cargo test");
    cx.simulate_keystrokes("cmd-alt-o");
    cx.run_until_parked();
    // One thin line: the name, then the facts after it on the same line.
    let first = shells[0].1.item;
    let label = cx.debug_bounds(selector("shapes-label", first)).expect("named");
    let meta = cx.debug_bounds(selector("shapes-meta", first)).expect("one meta line");
    assert!(meta.left() >= label.right() - px(0.5), "after the name: {meta:?} {label:?}");
    assert!((meta.center().y - label.center().y).abs() < px(1.0), "on its line");
    let lines = view.read_with(cx, |v, cx| {
        let item = v.item(shells[1].1).cloned();
        item.map(|item| v.tile_meta(&item, std::time::SystemTime::now(), cx).0)
    });
    assert_eq!(lines.as_deref(), Some("oss/slopty"));
    let ix = view.read_with(cx, |v, _| v.layout.active_workspace());
    assert!(cx.debug_bounds(leak(format!("overview-rollup-{ix}"))).is_some(), "its rollup");
}

//! The status bar's facts and its hosts popover, the inbox as a mailbox, and the palette's
//! rows with their context, in the headless workspace.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{Modifiers, MouseButton};
use slopty_client::tunnel::Forward;
use slopty_proto::orchestration::Port;

use super::*;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

/// Put the pointer over what `selector` names, so what shows only under it is drawn.
fn hover(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_mouse_move(at.center(), None, Modifiers::default());
    cx.run_until_parked();
}

/// The labels a screen reader reads in the last frame.
fn labels(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    tree.into_iter().filter_map(|n| n.label).collect()
}

/// A shell in a repository on a branch, focused.
fn shell_in_repo(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    cwd: &str,
    repo: &str,
    branch: &str,
) -> (SessionId, TileRef) {
    let session = SessionId::new();
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::Terminal { session },
        sleeping: false,
        name: None,
    };
    let tile = TileRef { worker: fake.key, item: item.id };
    let (key, me) = (fake.key, fake.me);
    let summary = SessionSummary {
        repo: Some(repo.to_owned()),
        branch: Some(branch.to_owned()),
        ..summary(session, Some(cwd))
    };
    view.update_in(cx, |v, _window, cx| {
        v.session_opened(key, summary, cx);
        v.apply_sync(key, ItemSync::Delta { version: 1, by: me, op: ItemOp::Upsert(item) }, cx);
    });
    cx.run_until_parked();
    (session, tile)
}

fn finished(command: &str, exit: u8) -> Finished {
    Finished { command: command.to_owned(), exit: Some(exit), elapsed: Duration::from_secs(40) }
}

/// The left of the bar says where the focused shell is: its worker, the repository and the
/// path within it, and the branch. The right counts the ports forwarded here, which list them,
/// and the workers; the frame time waits for the stream stats.
#[gpui::test]
fn the_status_bar_says_where_the_shell_is_and_counts_what_is_shared(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (shell, _tile) =
        shell_in_repo(&view, cx, &studio, "/w/oss/slopty/crates/ui", "/w/oss/slopty", "main");
    let port = |number| Port { number, pid: 2, process: "vite".to_owned(), session: Some(shell) };
    let forwards = vec![
        Forward { port: port(5173), local: Some(5173) },
        Forward { port: port(8080), local: Some(8081) },
    ];
    view.update_in(cx, |v, _window, cx| v.ports_changed(shell, forwards, cx));
    cx.run_until_parked();
    let names = labels(&view, cx);
    for readout in ["studio", "slopty/crates/ui", "branch main", "2 ports", "1 worker"] {
        assert!(names.iter().any(|l| l == readout), "{readout}: {names:#?}");
    }
    assert!(cx.debug_bounds("status-frame").is_none(), "no frame time without the stats");
    assert!(cx.debug_bounds("status-workers-dot").is_none(), "a worker that is up says nothing");

    click(cx, "status-ports");
    let lines = view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("the ports are listed");
        palette.read(cx).matches(cx).len()
    });
    assert_eq!(lines, 4, "a tile and a browser line for each port");
}

/// "N workers" wears a dot once a worker is not up, and opens the hosts popover: each worker
/// with its round trip or what is wrong, the app's connect and forget under the pointer, and
/// a way to add one. A row goes to its worker; a click elsewhere closes it.
#[gpui::test]
fn the_workers_count_opens_the_hosts_and_their_actions(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let (studio_key, laptop_key) = (studio.key, laptop.key);
    let (connected, forgot, added) =
        (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
    view.update_in(cx, |v, _w, cx| {
        v.set_rtt(studio_key, Some(Duration::from_micros(4_240)), cx);
        v.disconnect_worker(laptop_key, WorkerStatus::Reconnecting("lost".into()), cx);
        let run = |flag: &Rc<Cell<bool>>| -> MenuRun {
            let flag = Rc::clone(flag);
            Rc::new(move |_w, _cx| flag.set(true))
        };
        let hosts = [studio_key, laptop_key]
            .into_iter()
            .map(|k| {
                (k, HostActions { connect: Some(run(&connected)), forget: Some(run(&forgot)) })
            })
            .collect();
        v.set_host_actions(hosts, Some(run(&added)), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-workers-dot").is_some(), "the lost one shows");
    assert!(labels(&view, cx).iter().any(|l| l == "2 workers, 1 not connected"));

    click(cx, "status-workers");
    assert!(view.read_with(cx, |v, _| v.hosts_open()));
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "studio, 4.2 ms"), "its round trip: {names:#?}");
    assert!(names.iter().any(|l| l == "laptop, reconnecting"), "what is wrong: {names:#?}");

    // Connect is offered only where the link is down.
    hover(cx, leak(format!("hosts-row-{studio_key}")));
    assert!(cx.debug_bounds(leak(format!("hosts-connect-{studio_key}"))).is_none());
    hover(cx, leak(format!("hosts-row-{laptop_key}")));
    click(cx, leak(format!("hosts-connect-{laptop_key}")));
    assert!(connected.get(), "the app dials it");
    assert!(!view.read_with(cx, |v, _| v.hosts_open()), "and the popover goes");

    click(cx, "status-workers");
    hover(cx, leak(format!("hosts-row-{laptop_key}")));
    click(cx, leak(format!("hosts-forget-{laptop_key}")));
    assert!(forgot.get());
    click(cx, "status-workers");
    click(cx, "hosts-add");
    assert!(added.get());

    click(cx, "status-workers");
    let bar = cx.debug_bounds("statusbar").expect("the bar");
    cx.simulate_mouse_down(bar.origin, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(bar.origin, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.hosts_open()), "a click elsewhere closes it");
    click(cx, "status-workers");
    click(cx, leak(format!("hosts-row-{studio_key}")));
    assert!(!view.read_with(cx, |v, _| v.hosts_open()), "a row goes to its worker");
}

/// The inbox keeps what it was told: *Unread* lists what is still badged, *All* the history,
/// newest first. A row's age swaps for mark-read under the pointer; "Mark all read" reads the
/// rest; with nothing unread it says so.
#[gpui::test]
fn the_inbox_reads_like_a_mailbox(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (built, lint) = (SessionId::new(), SessionId::new());
    let _built = opens_in(&view, cx, &studio, built, studio.me, 1, Some("/w/oss/slopty"));
    let _lint = opens(&view, cx, &studio, lint, studio.me, 2);
    let _here = opens(&view, cx, &studio, SessionId::new(), studio.me, 3);
    view.update_in(cx, |v, _w, cx| {
        v.command_finished(built, finished("cargo build", 0), cx);
        v.command_finished(lint, finished("cargo clippy", 101), cx);
    });
    cx.run_until_parked();
    click(cx, "bell");
    assert!(cx.debug_bounds("inbox-unread").is_some() && cx.debug_bounds("inbox-all").is_some());
    let names = labels(&view, cx);
    assert!(
        names.iter().any(|l| l == "cargo build · Done · 40.0 s · studio · oss/slopty"),
        "two lines: the command, then its outcome, worker and directory: {names:#?}"
    );
    let (newest, older) = (
        cx.debug_bounds(leak(format!("inbox-finished-{lint}"))).expect("the lint's row"),
        cx.debug_bounds(leak(format!("inbox-finished-{built}"))).expect("the build's row"),
    );
    assert!(newest.top() < older.top(), "newest first");

    let read = leak(format!("inbox-finished-{built}-read"));
    hover(cx, leak(format!("inbox-finished-{built}")));
    click(cx, read);
    assert_eq!(view.read_with(cx, |v, _| v.inbox_count()), 1, "marked read");
    assert!(view.read_with(cx, |v, _| v.finished(built).is_none()), "its badge went with it");
    assert!(cx.debug_bounds(leak(format!("inbox-finished-{built}"))).is_none(), "not unread");

    click(cx, "inbox-mark-all");
    assert_eq!(view.read_with(cx, |v, _| v.inbox_count()), 0);
    assert!(cx.debug_bounds("inbox-empty").is_some(), "nothing unread says so");
    assert!(labels(&view, cx).iter().any(|l| l == inbox::ALL_CAUGHT_UP));
    assert!(cx.debug_bounds("inbox-mark-all").is_none(), "nothing left to mark");

    click(cx, "inbox-all");
    assert!(view.read_with(cx, |v, _| v.inbox_shows_all()));
    assert!(cx.debug_bounds("inbox-empty").is_none(), "the history keeps both");
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l.starts_with("cargo build · Done")), "{names:#?}");
    assert!(names.iter().any(|l| l.starts_with("cargo clippy · Exit 101")), "{names:#?}");
}

/// A tile's line says its name alone, where it is in a muted second column (the worker once
/// there are two, the directory), how long it has run on the right; its state takes the kind
/// icon's slot.
#[gpui::test]
fn a_palette_row_says_where_the_tile_is(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _laptop = connect(&view, cx, 2, "laptop");
    let waiting = SessionId::new();
    let _tile = opens_in(&view, cx, &studio, waiting, studio.me, 1, Some("/w/oss/slopty"));
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(waiting), cx));
    cx.run_until_parked();
    let lines = view.update(cx, |v, cx| v.palette_lines(cx));
    assert!(lines.iter().all(|l| !l.label.starts_with("Go to")), "{lines:#?}");
    let line = lines
        .iter()
        .find(|l| matches!(l.run, PaletteRun::Session(s) if s == waiting))
        .expect("the tile's line");
    assert_eq!(line.context(), "studio · oss/slopty");
    assert_eq!(line.status, Some(crate::icons::Status::NeedsYou));
    let found = crate::palette::filter("slopty", &lines);
    assert!(found.iter().any(|l| std::ptr::eq(*l, line)), "a directory finds its shells");

    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    let (row, context, title) = (
        cx.debug_bounds("palette-item-0").expect("the first row"),
        cx.debug_bounds("palette-context-0").expect("its context"),
        cx.debug_bounds("palette-title-0").expect("its title"),
    );
    assert!(title.right() <= context.left(), "after the title: {title:?} {context:?}");
    assert!(context.right() <= row.right(), "inside the row: {context:?} {row:?}");
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Image", Some("Needs you"))), "the mark leads: {tree:#?}");
}

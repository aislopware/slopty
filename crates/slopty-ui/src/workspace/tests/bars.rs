//! The status bar's facts and its hosts popover, the inbox as a mailbox, and the palette's
//! rows with their context, in the headless workspace.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{Modifiers, MouseButton};
use slopty_client::tunnel::Forward;
use slopty_proto::orchestration::Port;
use slopty_proto::tailnet::LinkPath;

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
        changes: None,
        ..summary(session, Some(cwd))
    };
    view.update_in(cx, |v, _window, cx| {
        v.session_opened(key, summary, cx);
        v.apply_sync(key, ItemSync::Delta { version: 1, by: me, op: ItemOp::Add(item) }, cx);
    });
    cx.run_until_parked();
    (session, tile)
}

/// Open or close the hosts popover as the "…" menu's Workers does, the count being absent
/// while every worker is up.
fn toggle_hosts(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) {
    view.update(cx, WorkspaceView::toggle_hosts);
    cx.run_until_parked();
}

fn finished(command: &str, exit: u8) -> Finished {
    Finished { command: command.to_owned(), exit: Some(exit), elapsed: Duration::from_secs(40) }
}

/// A workspace nobody named is named by where its first shell is: the repository, else the
/// directory; its number only where neither says anything (the home directory, no shell yet).
/// A name given wins.
#[gpui::test]
fn a_workspace_is_named_by_where_its_first_shell_is(cx: &mut TestAppContext) {
    use crate::workspace::tile::place_name;
    assert_eq!(place_name("/x/slopty/crates", Some("/x/slopty"), None).as_deref(), Some("slopty"));
    assert_eq!(place_name("/Users/me/src/app", None, None).as_deref(), Some("app"));
    assert_eq!(place_name("/Users/me", None, None), None, "home says nothing");
    assert_eq!(place_name("/", None, None), None);

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    assert_eq!(view.read_with(cx, |v, _| v.workspace_name()), "Workspace 1", "no shell yet");
    shell_in_repo(&view, cx, &studio, "/x/slopty/crates", "/x/slopty", "main");
    assert_eq!(view.read_with(cx, |v, _| v.workspace_name()), "slopty");
    assert!(labels(&view, cx).iter().any(|l| l == "slopty, 1 tile"), "the bar says it");
    view.update(cx, |v, cx| {
        v.layout.set_workspace_name(0, Some("release".to_owned()));
        cx.notify();
    });
    assert_eq!(view.read_with(cx, |v, _| v.workspace_name()), "release", "a given name wins");
}

/// The left of the bar says where the focused shell is, as one path: its worker (with one
/// worker too), the repository and the path within it, the branch, in that order. The right
/// counts the ports forwarded here, which list them; the workers go uncounted while every one
/// is up, and the frame time waits for the stream stats.
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
    for readout in ["studio", "slopty/crates/ui", "branch main", "2 ports"] {
        assert!(names.iter().any(|l| l == readout), "{readout}: {names:#?}");
    }
    let at = |cx: &mut VisualTestContext, selector: &'static str| {
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"))
    };
    let (worker, cwd, branch) =
        (at(cx, "status-worker"), at(cx, "status-cwd"), at(cx, "status-branch"));
    assert!(worker.right() < cwd.left() && cwd.right() < branch.left(), "one path, in order");
    assert!(cx.debug_bounds("status-frame").is_none(), "no frame time without the stats");
    assert!(cx.debug_bounds("status-workers").is_none(), "a worker that is up says nothing");

    click(cx, "status-ports");
    let lines = view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("the ports are listed");
        palette.read(cx).matches(cx).len()
    });
    assert_eq!(lines, 4, "a tile and a browser line for each port");
}

/// A quick round trip is said only under the pointer, and its samples draw nothing; a slow
/// one stands on its own. Either lands at the start of the bar's right, so nothing else there
/// moves, and the figure says its unit, not "RTT".
#[gpui::test]
fn a_quick_round_trip_waits_for_the_pointer_and_a_slow_one_stands(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (shell, _tile) =
        shell_in_repo(&view, cx, &studio, "/w/oss/slopty", "/w/oss/slopty", "main");
    let forward = Forward {
        port: Port { number: 5173, pid: 2, process: "vite".to_owned(), session: Some(shell) },
        local: Some(5173),
    };
    view.update_in(cx, |v, _window, cx| v.ports_changed(shell, vec![forward], cx));
    cx.run_until_parked();
    let key = studio.key;
    let sample = |cx: &mut VisualTestContext, micros: u64| {
        view.update_in(cx, |v, _w, cx| v.set_rtt(key, Some(Duration::from_micros(micros)), cx));
        cx.run_until_parked();
    };
    let ports = |cx: &mut VisualTestContext| cx.debug_bounds("status-ports").expect("the ports");
    let before = ports(cx);

    sample(cx, 4_240);
    assert!(cx.debug_bounds("status-rtt").is_none(), "a quick link says nothing");
    let drawn = view.read_with(cx, WorkspaceView::chrome_renders);
    sample(cx, 4_870);
    assert_eq!(view.read_with(cx, WorkspaceView::chrome_renders), drawn, "nor draws the bar");

    hover(cx, "statusbar");
    assert!(cx.debug_bounds("status-rtt").is_some(), "under the pointer it shows");
    assert!(labels(&view, cx).iter().any(|l| l == "Round trip 4.9 ms"), "the latest sample");
    assert_eq!(ports(cx), before, "the round trip moved the ports");
    let away = cx.debug_bounds("navigator").expect("the navigator").center();
    cx.simulate_mouse_move(away, None, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-rtt").is_none(), "and goes with it");

    sample(cx, 123_000);
    assert!(cx.debug_bounds("status-rtt").is_some(), "a slow link says so unasked");
    assert!(labels(&view, cx).iter().any(|l| l == "Round trip 123 ms"));
    assert_eq!(ports(cx), before, "{before:?}");
}

/// A tab's words sit on the bar's midline with its buttons, the active tab reaches down
/// through the bar's edge to join the content, and every tab keeps the same box, so switching
/// moves none.
#[gpui::test]
fn the_active_tab_joins_the_content_on_the_bars_midline(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &studio);
    // A column moved down makes a second workspace worth a tab.
    view.update_in(cx, |v, _w, cx| {
        v.tick();
        v.layout.move_column_to_workspace_down();
        v.after_focus_moved(cx);
        cx.notify();
    });
    cx.run_until_parked();
    let at = |cx: &mut VisualTestContext, selector: &'static str| {
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"))
    };
    let (bar, bell) = (at(cx, "titlebar"), at(cx, "bell"));
    let active = view.read_with(cx, |v, _| v.layout.active_workspace());
    for ix in [0, 1] {
        let (tab, band) =
            (at(cx, leak(format!("ws-tab-{ix}"))), at(cx, leak(format!("ws-tab-band-{ix}"))));
        let off = f32::from(band.center().y - bell.center().y).abs();
        assert!(off < 0.5, "tab {ix}'s words {off} pt off the buttons' midline");
        assert!(tab.bottom() >= bar.bottom(), "tab {ix} reaches the edge: {tab:?} {bar:?}");
    }
    let before = (at(cx, "ws-tab-0"), at(cx, "ws-tab-1"));
    click(cx, if active == 0 { "ws-tab-1" } else { "ws-tab-0" });
    assert_ne!(view.read_with(cx, |v, _| v.layout.active_workspace()), active, "switched");
    assert_eq!((at(cx, "ws-tab-0"), at(cx, "ws-tab-1")), before, "switching moved a tab");
}

/// "N workers" shows, with a dot, once a worker is not up, and opens the hosts popover: each worker
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

/// How the tailnet carries a link shows beside its round trip: in the status bar for the
/// focused worker, in the hosts popover for each, and in the navigator only for a DERP relay,
/// the slow path. It goes with the link, and a link the worker has said nothing of shows none.
#[gpui::test]
fn the_link_path_shows_beside_the_round_trip_and_goes_with_the_link(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let lan = connect(&view, cx, 3, "lan");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let (studio_key, laptop_key, lan_key) = (studio.key, laptop.key, lan.key);
    view.update_in(cx, |v, _w, cx| {
        v.set_rtt(studio_key, Some(Duration::from_micros(4_240)), cx);
        v.set_link_path(studio_key, LinkPath::Direct, cx);
        v.set_link_path(laptop_key, LinkPath::Derp { region: "fra".to_owned() }, cx);
    });
    cx.run_until_parked();
    let names = labels(&view, cx);
    assert!(!names.iter().any(|l| l == "Direct"), "a quick direct link is quiet: {names:#?}");
    assert!(names.iter().any(|l| l == "laptop, DERP · fra"), "a relay is named: {names:#?}");
    hover(cx, "statusbar");
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "Direct"), "the focused worker's path: {names:#?}");
    assert!(cx.debug_bounds(leak(format!("nav-path-{studio_key}"))).is_none(), "direct is quiet");
    assert!(cx.debug_bounds(leak(format!("nav-path-{laptop_key}"))).is_some());

    toggle_hosts(&view, cx);
    let names = labels(&view, cx);
    for row in ["studio, Direct, 4.2 ms", "laptop, DERP · fra", "lan"] {
        assert!(names.iter().any(|l| l == row), "{row}: {names:#?}");
    }
    assert!(cx.debug_bounds(leak(format!("hosts-path-{lan_key}"))).is_none(), "nothing said");
    toggle_hosts(&view, cx);

    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(laptop_key, WorkerStatus::Reconnecting("lost".into()), cx);
        v.set_link_path(laptop_key, LinkPath::PeerRelay, cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.link_path(laptop_key).cloned()), None, "went with it");
    assert!(cx.debug_bounds(leak(format!("nav-path-{laptop_key}"))).is_none());
    let _again = connect(&view, cx, 2, "laptop");
    assert_eq!(view.read_with(cx, |v, _| v.link_path(laptop_key).cloned()), None, "not yet said");
    view.update_in(cx, |v, _w, cx| v.set_link_path(laptop_key, LinkPath::PeerRelay, cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.link_path(laptop_key).cloned()),
        Some(LinkPath::PeerRelay)
    );

    let label = |path| navigator::path_label(&path);
    assert_eq!(label(LinkPath::Direct), ("Direct".to_owned(), false));
    assert_eq!(label(LinkPath::PeerRelay), ("Peer relay".to_owned(), false));
    let derp = |region: &str| LinkPath::Derp { region: region.to_owned() };
    assert_eq!(label(derp("fra")), ("DERP · fra".to_owned(), true), "the slow path");
    assert_eq!(label(derp("")), ("DERP".to_owned(), true), "no region to name");
}

/// A worker that turns this device away says so in a word wherever its link's health shows,
/// and in full when it is gone to.
#[gpui::test]
fn a_worker_the_tailnet_policy_closes_says_not_granted(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.disconnect_worker(key, WorkerStatus::NotGranted, cx));
    cx.run_until_parked();
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "Not granted"), "the status bar: {names:#?}");
    assert!(names.iter().any(|l| l == "studio, not granted"), "the navigator: {names:#?}");
    assert_eq!(WorkerStatus::NotGranted.text(), "closed to this device by the tailnet policy");
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

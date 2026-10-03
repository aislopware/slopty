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
        facts: BTreeMap::new(),
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
/// directory. Where neither says anything (a shell at home), its first tile's worker, never a
/// number; with nothing on it, it is new. A name given wins.
#[gpui::test]
fn a_workspace_is_named_by_where_its_first_shell_is(cx: &mut TestAppContext) {
    use crate::workspace::tile::place_name;
    assert_eq!(place_name("/x/slopty/crates", Some("/x/slopty"), None).as_deref(), Some("slopty"));
    assert_eq!(place_name("/Users/me/src/app", None, None).as_deref(), Some("app"));
    assert_eq!(place_name("/Users/me", None, None), None, "home says nothing");
    assert_eq!(place_name("/", None, None), None);

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    assert_eq!(view.read_with(cx, |v, _| v.workspace_name()), "New workspace", "nothing yet");
    let _home = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let name = view.read_with(cx, |v, _| v.workspace_name());
    assert_eq!(name, "studio", "a shell at home leaves it its worker's");
    shell_in_repo(&view, cx, &studio, "/x/slopty/crates", "/x/slopty", "main");
    assert_eq!(view.read_with(cx, |v, _| v.workspace_name()), "slopty");
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "slopty"), "the breadcrumb says it: {names:#?}");
    view.update(cx, |v, cx| {
        v.layout.set_workspace_name(0, Some("release".to_owned()));
        cx.notify();
    });
    assert_eq!(view.read_with(cx, |v, _| v.workspace_name()), "release", "a given name wins");
}

/// The left of the status bar says which machine the focused shell is on (with one worker
/// too) and nothing more: the checkout and branch are the breadcrumb's. The right counts the
/// ports forwarded here, which list them; the workers go uncounted while every one is up, and
/// the frame time waits for the stream stats.
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
    for readout in ["studio", "2 ports"] {
        assert!(names.iter().any(|l| l == readout), "{readout}: {names:#?}");
    }
    let place = cx.debug_bounds("status-place").expect("the left");
    assert!(cx.debug_bounds("status-worker").is_some_and(|w| place.contains(&w.center())));
    assert!(cx.debug_bounds("status-cwd").is_none(), "the directory is the header's");
    assert!(cx.debug_bounds("status-branch").is_none(), "the branch is the breadcrumb's");
    assert!(cx.debug_bounds("status-frame").is_none(), "no frame time without the stats");
    assert!(cx.debug_bounds("status-workers").is_none(), "a worker that is up says nothing");

    click(cx, "status-ports");
    let lines = view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("the ports are listed");
        palette.read(cx).matches().len()
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

/// The breadcrumb says where the focused shell is, `workspace / checkout / branch`, in that
/// order on the bar's midline with its buttons, the branch's changes after it. A workspace
/// named after the checkout leaves the checkout unsaid; another repository's says its name. The
/// checkout opens a menu only when the same repository is checked out elsewhere in the layout:
/// then its rows name each checkout's worker and go to a shell in it.
#[gpui::test]
fn the_breadcrumb_names_the_checkout_and_its_branch(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (session, tile) =
        shell_in_repo(&view, cx, &studio, "/w/oss/slopty/crates", "/w/oss/slopty", "main");
    let repo_id = slopty_proto::terminal::RepoId {
        origin: Some("github.com/aislopware/slopty".to_owned()),
        ..Default::default()
    };
    let key = studio.key;
    let id = repo_id.clone();
    view.update_in(cx, |v, _window, cx| {
        let summary = SessionSummary {
            repo: Some("/w/oss/slopty".to_owned()),
            repo_id: Some(id),
            branch: Some("main".to_owned()),
            changes: Some(slopty_proto::terminal::RepoChanges { files: 2, added: 25, removed: 15 }),
            ..summary(session, Some("/w/oss/slopty/crates"))
        };
        v.session_opened(key, summary, cx);
    });
    cx.run_until_parked();
    let at = |cx: &mut VisualTestContext, selector: &'static str| {
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"))
    };
    assert!(cx.debug_bounds("crumb-checkout").is_none(), "the workspace already says slopty");
    assert!(cx.debug_bounds("crumb-worker").is_none(), "one machine: no worker to name");
    let (ws, branch, changes, bell) = (
        at(cx, "crumb-workspace"),
        at(cx, "crumb-branch"),
        at(cx, "crumb-changes"),
        at(cx, "bell"),
    );
    assert!(ws.right() <= branch.left(), "in order");
    assert!(branch.contains(&changes.center()), "the changes follow the branch");
    for part in [ws, branch] {
        let off = f32::from(part.center().y - bell.center().y).abs();
        assert!(off < 0.5, "{off} pt off the buttons' midline");
    }
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "branch main"), "{names:#?}");

    // A shell in another repository says its checkout: words, with nothing to choose. Its
    // project is on one machine, so no machine is named in the breadcrumb; the tile's header
    // names it, since the workspace now holds two machines' tiles.
    let mini = connect(&view, cx, 3, "mini");
    let (_, notes) = shell_in_repo(&view, cx, &mini, "/w/notes", "/w/notes", "draft");
    view.update_in(cx, |v, _w, cx| v.focus_tile(notes, cx));
    cx.run_until_parked();
    let (ws, checkout, branch) =
        (at(cx, "crumb-workspace"), at(cx, "crumb-checkout"), at(cx, "crumb-branch"));
    assert!(ws.right() <= checkout.left() && checkout.right() <= branch.left(), "in order");
    assert!(cx.debug_bounds("crumb-worker").is_none(), "a project on one machine");
    assert!(cx.debug_bounds(selector("worker", notes.item)).is_some(), "the header says mini");
    let padding = 2.0 * Theme::default().spacing.sm;
    let width = f32::from(checkout.size.width);
    assert!(width > padding + 10.0, "the checkout's name is drawn, not only its padding: {width}");
    let roles = cx
        .update(|window, _cx| crate::a11y::tree(window))
        .into_iter()
        .filter(|n| n.label.as_deref() == Some("notes"))
        .map(|n| n.role)
        .collect::<Vec<_>>();
    assert!(roles.iter().any(|r| r == "Label"), "one checkout: words, no menu: {roles:?}");
    click(cx, "crumb-checkout");
    assert!(cx.debug_bounds("menu").is_none(), "nothing to choose, nothing opens");

    // The same repository on another worker makes the project span two machines: the
    // breadcrumb names the focused tile's, and its menu lists the clones by machine.
    let laptop = connect(&view, cx, 2, "laptop");
    let (far, far_tile) =
        shell_in_repo(&view, cx, &laptop, "/home/me/slopty-wt", "/home/me/slopty-wt", "fix");
    let laptop_key = laptop.key;
    view.update_in(cx, |v, _window, cx| {
        let summary = SessionSummary {
            repo: Some("/home/me/slopty-wt".to_owned()),
            repo_id: Some(repo_id),
            branch: Some("fix".to_owned()),
            ..summary(far, Some("/home/me/slopty-wt"))
        };
        v.session_opened(laptop_key, summary, cx);
    });
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    assert!(labels(&view, cx).iter().any(|l| l == "on studio"), "the focused tile's machine");
    assert!(cx.debug_bounds("crumb-checkout").is_none(), "the workspace already says slopty");
    click(cx, "crumb-worker");
    assert!(cx.debug_bounds("menu-studio").is_some(), "this clone, by its machine");
    click(cx, "menu-laptop");
    assert_eq!(focused(&view, cx), Some(far_tile), "a row goes to a shell in it");
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "branch fix"), "the crumbs follow: {names:#?}");
    assert!(names.iter().any(|l| l == "on laptop"), "and the machine: {names:#?}");
}

/// A tile's header names its worker only where its workspace holds tiles of more than one: a
/// workspace on one machine never pays for the chip, whatever else is connected.
#[gpui::test]
fn the_header_chip_shows_only_where_the_workspace_spans_machines(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let (_, here) = shell_in_repo(&view, cx, &studio, "/w/a", "/w/a", "main");
    assert!(cx.debug_bounds(selector("worker", here.item)).is_none(), "one machine here");
    new_workspace_from_the_bar(cx);
    let (_, there) = shell_in_repo(&view, cx, &laptop, "/w/b", "/w/b", "main");
    assert!(cx.debug_bounds(selector("worker", there.item)).is_none(), "nor in its own");
    view.update_in(cx, |v, _w, cx| {
        v.tick();
        v.layout.move_column_to_workspace_up();
        v.after_focus_moved(cx);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("worker", there.item)).is_some(), "two machines meet");
    assert!(cx.debug_bounds(selector("worker", here.item)).is_some(), "both say theirs");
}

/// A worker's actions in the hosts popover wait for the pointer, and the keyboard brings
/// them too: an action that holds the focus is drawn with its ring. A screen reader finds
/// them in the tree either way, buttons with their names.
#[gpui::test]
fn the_keyboard_reaches_a_workers_actions(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    // The popover lands at once, so a ring is drawn at its full strength.
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    let laptop = connect(&view, cx, 1, "laptop");
    let laptop_key = laptop.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(laptop_key, WorkerStatus::Reconnecting("lost".into()), cx);
        let none: MenuRun = Rc::new(|_w, _cx| {});
        let actions = HostActions {
            connect: Some(Rc::clone(&none)),
            forget: Some(Rc::clone(&none)),
            wake: None,
        };
        v.set_host_actions(std::iter::once((laptop_key, actions)).collect(), None, cx);
    });
    cx.run_until_parked();
    click(cx, "status-workers");
    let nodes = tree(cx);
    for name in ["Connect", "Forget"] {
        assert!(nodes.iter().any(|n| n.is("Button", Some(name))), "{name}: {nodes:#?}");
    }
    let actions = ["connect", "forget"]
        .map(|part| cx.debug_bounds(leak(format!("hosts-{part}-{laptop_key}"))).expect("laid out"));
    let ring =
        crate::colors::hsla_alpha(Theme::default().surfaces.accent, slopty_theme::alpha::STRONG);
    let ringed = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        let (scale, quads) = cx.update(|w, _| (w.scale_factor(), w.painted_quads()));
        // A stop's ring stands two of its widths clear all round, as `a11y::tab_stop` draws it.
        let around =
            |b: &Bounds<Pixels>| 4.0_f32.mul_add(crate::a11y::RING, f32::from(b.size.width));
        quads.iter().any(|q| {
            let at = point(px(q.bounds.center().x.0 / scale), px(q.bounds.center().y.0 / scale));
            let wide = q.bounds.size.width.0 / scale;
            q.border_color == ring
                && actions.iter().any(|b| b.contains(&at) && (wide - around(b)).abs() < 0.5)
        })
    };
    assert!(!ringed(cx), "at rest, nothing of them is drawn");
    // Along the ring a stop at a time, the keyboard the last input (a key that moves
    // nothing), until an action holds the focus.
    for _ in 0..16 {
        if ringed(cx) {
            break;
        }
        cx.update(Window::focus_next);
        cx.simulate_keystrokes("f19");
    }
    assert!(ringed(cx), "an action focused from the keyboard is drawn, with its ring");
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
                let actions = HostActions {
                    connect: Some(run(&connected)),
                    forget: Some(run(&forgot)),
                    wake: None,
                };
                (k, actions)
            })
            .collect();
        v.set_host_actions(hosts, Some(run(&added)), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-workers-dot").is_some(), "the lost one shows");
    assert!(labels(&view, cx).iter().any(|l| l == "2 machines, 1 not connected"));

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
    assert!(
        !names.iter().any(|l| l == "laptop, DERP · fra"),
        "a relay that has not held is not news yet: {names:#?}"
    );
    hover(cx, "statusbar");
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "Direct"), "the focused worker's path: {names:#?}");
    assert!(cx.debug_bounds(leak(format!("nav-path-{studio_key}"))).is_none(), "direct is quiet");
    assert!(cx.debug_bounds(leak(format!("nav-path-{laptop_key}"))).is_none());

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

/// A link on a DERP relay is said only once the relay has held for
/// [`slopty_proto::tailnet::DERP_NOTICE_AFTER`], since a path starts there while a direct one is
/// found. Then the status bar says it in words, in the muted tone, with the fix under the
/// pointer, and the navigator names the relay. A direct path takes both away.
#[gpui::test]
fn a_link_that_stays_on_derp_is_said_once_it_has_held(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let key = studio.key;
    let nav = leak(format!("nav-path-{key}"));
    let derp = || LinkPath::Derp { region: "fra".to_owned() };
    view.update_in(cx, |v, _w, cx| v.set_link_path(key, derp(), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-relay").is_none(), "a path still settling says nothing");
    assert!(cx.debug_bounds(nav).is_none());

    let almost = slopty_proto::tailnet::DERP_NOTICE_AFTER.saturating_sub(Duration::from_secs(1));
    cx.executor().advance_clock(almost);
    // The worker says the path again: the relay's clock is not restarted by it.
    view.update_in(cx, |v, _w, cx| v.set_link_path(key, derp(), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-relay").is_none(), "nine seconds are not enough");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "Relayed via fra — adds latency"), "said: {names:#?}");
    assert!(names.iter().any(|l| l == "studio, DERP · fra"), "the navigator: {names:#?}");
    assert!(cx.debug_bounds(nav).is_some());
    let fix = view.read_with(cx, |v, cx| v.relay_notice(key, cx).map(|n| n.fix));
    assert_eq!(fix, Some(LinkPath::relay_fix()));

    view.update_in(cx, |v, _w, cx| v.set_link_path(key, LinkPath::Direct, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-relay").is_none(), "a direct path takes it away");
    assert!(cx.debug_bounds(nav).is_none());
}

/// A worker the app can wake gets "Wake <name>" among the palette's commands and a Wake in its
/// hosts row, and either runs the app's wake; a worker the app cannot wake gets neither.
#[gpui::test]
fn a_sleeping_worker_is_woken_from_the_palette_and_its_hosts_row(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let (studio_key, laptop_key) = (studio.key, laptop.key);
    let woke = Rc::new(Cell::new(0_u32));
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(laptop_key, WorkerStatus::Unreachable, cx);
        let count = Rc::clone(&woke);
        let wake: MenuRun = Rc::new(move |_w, _cx| count.set(count.get().saturating_add(1)));
        let hosts = [
            (studio_key, HostActions::default()),
            (laptop_key, HostActions { wake: Some(wake), ..HostActions::default() }),
        ]
        .into_iter()
        .collect();
        v.set_host_actions(hosts, None, cx);
    });
    cx.run_until_parked();
    let wakes: Vec<(String, crate::palette::Section)> = view.update(cx, |v, cx| {
        let lines = v.palette_lines(cx);
        lines
            .into_iter()
            .filter(|l| matches!(l.run, PaletteRun::Wake(_)))
            .map(|l| (l.label, l.section))
            .collect()
    });
    assert_eq!(wakes, [("Wake laptop".to_owned(), crate::palette::Section::Commands)]);

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_input("Wake laptop");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(woke.get(), 1, "the palette ran the app's wake");

    toggle_hosts(&view, cx);
    hover(cx, leak(format!("hosts-row-{studio_key}")));
    assert!(cx.debug_bounds(leak(format!("hosts-wake-{studio_key}"))).is_none());
    hover(cx, leak(format!("hosts-row-{laptop_key}")));
    click(cx, leak(format!("hosts-wake-{laptop_key}")));
    assert_eq!(woke.get(), 2, "and so did the row");
}

/// The palette stops sharing the clipboard with one machine and shares it again, by its
/// name: the navigator marks the machine it is not shared with, the app is told to keep the
/// choice, and the other machine is left as it was.
#[gpui::test]
fn the_clipboard_is_stopped_and_shared_with_one_machine_from_the_palette(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let (studio_key, laptop_key) = (studio.key, laptop.key);
    let events = Rc::new(std::cell::RefCell::new(Vec::new()));
    let heard = Rc::clone(&events);
    cx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &WorkspaceEvent, _cx| {
            if matches!(event, WorkspaceEvent::ClipboardShared { .. }) {
                heard.borrow_mut().push(*event);
            }
        })
        .detach();
    });
    view.update_in(cx, |v, _w, cx| {
        let sharing = slopty_settings::ClipboardSettings { sync: true, ..v.clip_sharing.clone() };
        v.set_clipboard_sharing(sharing, cx);
    });
    cx.run_until_parked();
    let lines = |view: &Entity<WorkspaceView>, cx: &mut VisualTestContext| -> Vec<String> {
        view.update(cx, |v, cx| {
            let mut lines: Vec<String> = v
                .palette_lines(cx)
                .into_iter()
                .map(|l| l.label)
                .filter(|l| l.contains("the clipboard with"))
                .collect();
            lines.sort();
            lines
        })
    };
    assert_eq!(
        lines(&view, cx),
        ["Stop sharing the clipboard with laptop", "Stop sharing the clipboard with studio"]
    );
    let off = |cx: &mut VisualTestContext, key: WorkerKey| {
        cx.debug_bounds(leak(format!("nav-clip-off-{key}"))).is_some()
    };
    assert!(!off(cx, laptop_key) && !off(cx, studio_key), "shared: nothing to mark");

    let run = |cx: &mut VisualTestContext, line: &str| {
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_input(line);
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    };
    run(cx, "Stop sharing the clipboard with laptop");
    assert!(!view.read_with(cx, |v, _| v.clipboard_shared(laptop_key)));
    assert!(view.read_with(cx, |v, _| v.clipboard_shared(studio_key)), "the other is kept");
    assert!(off(cx, laptop_key) && !off(cx, studio_key), "the navigator marks the laptop");
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("The clipboard is no longer shared with laptop")
    );
    assert_eq!(
        lines(&view, cx),
        ["Share the clipboard with laptop", "Stop sharing the clipboard with studio"]
    );

    run(cx, "Share the clipboard with laptop");
    assert!(view.read_with(cx, |v, _| v.clipboard_shared(laptop_key)));
    assert!(!off(cx, laptop_key), "shared again, unmarked");
    assert_eq!(
        events.borrow().as_slice(),
        [
            WorkspaceEvent::ClipboardShared { worker: laptop_key, share: false },
            WorkspaceEvent::ClipboardShared { worker: laptop_key, share: true },
        ],
        "the app keeps each choice"
    );
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
        names.iter().any(|l| l == "cargo build · Done · 40 s · oss/slopty"),
        "two lines: the command, then its outcome and directory, the lone worker unsaid: {names:#?}"
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

/// A long command that ends on the focused tile while the app is away was not watched: it
/// enters the inbox and counts in the badge, as the note that went out for it says. With the
/// app in front, the same end on the focused tile is seen as it happens and leaves no row.
#[gpui::test]
fn a_command_ending_on_the_focused_tile_while_away_enters_the_inbox(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (seen, missed) = (SessionId::new(), SessionId::new());
    let first = opens(&view, cx, &studio, seen, studio.me, 1);
    let second = opens(&view, cx, &studio, missed, studio.me, 2);
    view.update_in(cx, |v, _w, cx| {
        v.focus_tile(first, cx);
        v.command_finished(seen, finished("cargo build", 0), cx);
        v.focus_tile(second, cx);
        v.set_app_active(false, cx);
        v.command_finished(missed, finished("cargo test", 0), cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert!(v.finished(seen).is_none(), "watched in front: no row");
        assert!(v.finished(missed).is_some(), "on the focused tile, but the app was away");
        assert_eq!(v.inbox_count(), 1);
    });
}

/// A Claude Code thread on `fake` whose status line said `limits`, with no terminal.
fn plan_row(limits: Vec<slopty_proto::thread::Limit>) -> slopty_proto::thread::wire::ThreadRow {
    let mut row = crate::conversation::thread::fixtures::thread("edit").row(WallMs::now());
    row.id = slopty_proto::thread::ThreadId::new();
    row.terminal = None;
    row.agent = slopty_proto::thread::AgentId(slopty_proto::thread::AgentId::CLAUDE_CODE.into());
    row.meters.limits = limits;
    row
}

fn window(name: &str, used_bp: u32) -> slopty_proto::thread::Limit {
    slopty_proto::thread::Limit { name: name.to_owned(), used_bp, resets_ms: None }
}

/// The bar says the focused tile's machine's plan windows as its agents published them, and a
/// click lists every machine's readings. A machine whose
/// agents published none shows no meter.
#[gpui::test]
fn the_status_bar_shows_the_focused_machines_plan_windows(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let here = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let there = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 1);
    let row = plan_row(vec![window("five-hour", 2_300), window("seven-day", 4_100)]);
    let table = slopty_proto::thread::wire::TableFrame::Snapshot {
        cursor: slopty_proto::thread::Cursor { epoch: 1, seq: 1 },
        rows: vec![row],
    };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        v.thread_table(key, &table, cx);
        v.focus_tile(here, cx);
    });
    cx.run_until_parked();
    assert!(labels(&view, cx).iter().any(|l| l == "Plan usage 5h 23% · 7d 41%"));

    click(cx, "status-plan");
    assert!(cx.debug_bounds("plans").is_some(), "every reading, listed");
    let row = "studio · Claude Code, 5h 23% · 7d 41% · now".to_owned();
    assert!(labels(&view, cx).contains(&row), "{:?}", labels(&view, cx));

    view.update_in(cx, |v, _w, cx| v.focus_tile(there, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-plan").is_none(), "the laptop's agents said nothing");
}

/// A worker on another build shows Update where it is named, not only on its tiles: on its
/// navigator row, at rest, and in the hosts popover. Each runs the app's update against the
/// host the notice names; with one under way, neither offers it again.
#[gpui::test]
fn a_worker_on_another_build_offers_update_where_it_is_named(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let asked: Rc<std::cell::RefCell<Vec<String>>> = Rc::default();
    let seen = Rc::clone(&asked);
    cx.update(|_w, cx| {
        let start: crate::add_worker::Update =
            Rc::new(move |host: &str, _w, _cx| seen.borrow_mut().push(host.to_owned()));
        cx.set_global(crate::add_worker::Updates { start: Some(start), ..Default::default() });
    });
    let key = studio.key;
    let notice =
        UpdateNotice { of: Of::Worker, host: "studio.ts.net".to_owned(), peer: "0.0.9".to_owned() };
    view.update_in(cx, |v, _w, cx| v.disconnect_worker(key, WorkerStatus::NeedsUpdate(notice), cx));
    cx.run_until_parked();

    click(cx, leak(format!("nav-update-{key}")));
    cx.run_until_parked();
    assert_eq!(*asked.borrow(), ["studio.ts.net"], "the navigator's Update");

    click(cx, "status-workers");
    let update = leak(format!("hosts-update-{key}"));
    assert!(cx.debug_bounds(update).is_some(), "shown at rest in the popover");
    click(cx, update);
    cx.run_until_parked();
    assert_eq!(asked.borrow().len(), 2, "the popover's Update");

    cx.update(|_w, cx| {
        let install = crate::add_worker::Install {
            steps: Vec::new(),
            bar: crate::add_worker::Bar::Busy,
            failed: None,
        };
        cx.global_mut::<crate::add_worker::Updates>().runs.insert("studio.ts.net".into(), install);
    });
    view.update(cx, |_v, cx| cx.notify());
    cx.run_until_parked();
    assert!(cx.debug_bounds(leak(format!("nav-update-{key}"))).is_none(), "not while it runs");
}

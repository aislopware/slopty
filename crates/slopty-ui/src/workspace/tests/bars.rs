//! The title bar's readouts, a machine's menu, a finish's badge, and the palette's rows with
//! their context, in the headless workspace.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{Modifiers, MouseButton};
use slopty_client::tunnel::Forward;
use slopty_proto::orchestration::Port;
use slopty_proto::tailnet::LinkPath;

use super::*;
use crate::palette::PaletteRun;

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

fn finished(command: &str, exit: u8) -> Finished {
    Finished { command: command.to_owned(), exit: Some(exit), elapsed: Duration::from_secs(40) }
}

/// A shell's place is its repository, else its directory, and nothing at home: what a project
/// in a repository is called.
#[test]
fn a_place_is_named_by_its_repository_else_its_directory() {
    use crate::workspace::tile::place_name;
    assert_eq!(place_name("/x/slopty/crates", Some("/x/slopty"), None).as_deref(), Some("slopty"));
    assert_eq!(place_name("/Users/me/src/app", None, None).as_deref(), Some("app"));
    assert_eq!(place_name("/Users/me", None, None), None, "home says nothing");
    assert_eq!(place_name("/", None, None), None);
}

/// The title bar's readouts count the ports forwarded here, which list them, before the bell;
/// the machine a shell runs on is the navigator's and the breadcrumb's to say, not theirs, and
/// the frame time waits for the stream stats.
#[gpui::test]
fn the_readouts_count_what_is_shared_and_say_no_machine(cx: &mut TestAppContext) {
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
    assert!(names.iter().any(|l| l == "2 ports"), "{names:#?}");
    let titlebar = cx.debug_bounds("titlebar").expect("the title bar");
    let ports = cx.debug_bounds("readout-ports").expect("the ports");
    let bell = cx.debug_bounds("bell").expect("the bell");
    assert!(titlebar.contains(&ports.center()) && ports.right() <= bell.left(), "{ports:?}");
    assert!(cx.debug_bounds("status-worker").is_none(), "the machine is the navigator's");
    assert!(cx.debug_bounds("readout-frame").is_none(), "no frame time without the stats");

    click(cx, "readout-ports");
    let lines = view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("the ports are listed");
        palette.read(cx).matches().len()
    });
    assert_eq!(lines, 4, "a tile and a browser line for each port");
}

/// A newer Slopty is said quietly at the title bar's end until this build is the latest, and
/// the line opens its release page.
#[gpui::test]
fn a_newer_release_is_said_in_the_bar_and_opens_its_page(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _studio = connect(&view, cx, 1, "studio");
    assert!(cx.debug_bounds("readout-release").is_none(), "nothing while this is the latest");
    let page = "https://github.com/aislopware/slopty/releases/tag/v0.2.0";
    let release =
        slopty_client::update::Release { version: "0.2.0".to_owned(), page: page.to_owned() };
    view.update(cx, |v, cx| v.set_release(Some(release), cx));
    cx.run_until_parked();
    assert!(labels(&view, cx).iter().any(|l| l == "Slopty 0.2.0 is out"));
    let readouts = cx.debug_bounds("readouts").expect("the readouts");
    assert!(cx.debug_bounds("readout-release").is_some_and(|r| readouts.contains(&r.center())));
    click(cx, "readout-release");
    assert_eq!(cx.opened_url().as_deref(), Some(page));
    view.update(cx, |v, cx| v.set_release(None, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("readout-release").is_none(), "gone once this build is the latest");
}

/// A round trip's samples draw no chrome while nothing shows them: the navigator names only a
/// slow link, and no readout repeats it.
#[gpui::test]
fn a_quick_round_trip_draws_no_chrome(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (_shell, _tile) =
        shell_in_repo(&view, cx, &studio, "/w/oss/slopty", "/w/oss/slopty", "main");
    let key = studio.key;
    let sample = |cx: &mut VisualTestContext, micros: u64| {
        view.update_in(cx, |v, _w, cx| v.set_rtt(key, Some(Duration::from_micros(micros)), cx));
        cx.run_until_parked();
    };
    sample(cx, 4_240);
    let drawn = view.read_with(cx, WorkspaceView::chrome_renders);
    sample(cx, 4_870);
    assert_eq!(view.read_with(cx, WorkspaceView::chrome_renders), drawn, "a quick link is no news");
    assert!(cx.debug_bounds("readouts").is_none(), "and no readout says it");
}

/// The breadcrumb says where the focused shell is, `project / checkout / branch`, in that
/// order on the bar's midline with its buttons, the branch's changes after it. A project
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
    assert!(cx.debug_bounds("crumb-checkout").is_none(), "the project already says slopty");
    assert!(cx.debug_bounds("crumb-worker").is_none(), "one machine: no worker to name");
    let (ws, branch, changes, bell) =
        (at(cx, "crumb-project"), at(cx, "crumb-branch"), at(cx, "crumb-changes"), at(cx, "bell"));
    assert!(ws.right() <= branch.left(), "in order");
    assert!(branch.contains(&changes.center()), "the changes follow the branch");
    for part in [ws, branch] {
        let off = f32::from(part.center().y - bell.center().y).abs();
        assert!(off < 0.5, "{off} pt off the buttons' midline");
    }
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "branch main"), "{names:#?}");

    // A shell in another repository says its checkout: words, with nothing to choose. Its
    // own group is on one machine, so no machine is named in the breadcrumb; the tile's
    // header names it, since its project now holds two machines' tiles.
    let mini = connect(&view, cx, 3, "mini");
    let (_, notes) = shell_in_repo(&view, cx, &mini, "/w/notes", "/w/notes", "draft");
    view.update_in(cx, |v, _w, cx| v.focus_tile(notes, cx));
    cx.run_until_parked();
    let (ws, checkout, branch) =
        (at(cx, "crumb-project"), at(cx, "crumb-checkout"), at(cx, "crumb-branch"));
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
    assert!(cx.debug_bounds("crumb-checkout").is_none(), "the project already says slopty");
    click(cx, "crumb-worker");
    assert!(cx.debug_bounds("menu-studio").is_some(), "this clone, by its machine");
    click(cx, "menu-laptop");
    assert_eq!(focused(&view, cx), Some(far_tile), "a row goes to a shell in it");
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "branch fix"), "the crumbs follow: {names:#?}");
    assert!(names.iter().any(|l| l == "on laptop"), "and the machine: {names:#?}");
}

/// A tile's header names its worker only where its project holds tiles of more than one: a
/// project on one machine never pays for the chip, whatever else is connected.
#[gpui::test]
fn the_header_chip_shows_only_where_the_project_spans_machines(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let (_, here) = shell_in_repo(&view, cx, &studio, "/w/a", "/w/a", "main");
    assert!(cx.debug_bounds(selector("worker", here.item)).is_none(), "one machine here");
    let (_, there) = shell_in_repo(&view, cx, &laptop, "/w/b", "/w/b", "main");
    assert!(cx.debug_bounds(selector("worker", there.item)).is_some(), "two machines meet");
    assert!(cx.debug_bounds(selector("worker", here.item)).is_some(), "both say theirs");
    view.update_in(cx, |v, _w, cx| {
        let home = slopty_client::layout::GroupKey::machine(there.worker);
        v.layout_action(cx, |l| l.move_to_project(there, &home));
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("worker", there.item)).is_none(), "alone in its own");
}

/// A machine's "…" waits for the pointer, and the keyboard brings it too: drawn with its ring
/// while it holds the focus. A screen reader finds it in the tree either way, a button named
/// for the machine.
#[gpui::test]
fn the_keyboard_reaches_a_machines_menu(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    let laptop = connect(&view, cx, 1, "laptop");
    let laptop_key = laptop.key;
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Button", Some("Machine actions, laptop"))), "{nodes:#?}");
    let button = cx.debug_bounds(leak(format!("nav-machine-menu-{laptop_key}"))).expect("laid out");
    let ring = crate::colors::hsla(Theme::default().surfaces.focus);
    let ringed = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        let (scale, quads) = cx.update(|w, _| (w.scale_factor(), w.painted_quads()));
        // A stop's ring stands its gap and its width clear all round, as `a11y::tab_stop` draws it.
        let reach = crate::a11y::RING + slopty_theme::stroke::FOCUS;
        let around = 2.0_f32.mul_add(reach, f32::from(button.size.width));
        quads.iter().any(|q| {
            let at = point(px(q.bounds.center().x.0 / scale), px(q.bounds.center().y.0 / scale));
            let wide = q.bounds.size.width.0 / scale;
            q.border_color == ring && button.contains(&at) && (wide - around).abs() < 0.5
        })
    };
    assert!(!ringed(cx), "at rest, nothing of it is drawn");
    // Along the ring a stop at a time, the keyboard the last input (a key that moves
    // nothing), until the menu's button holds the focus.
    for _ in 0..16 {
        if ringed(cx) {
            break;
        }
        cx.update(Window::focus_next);
        cx.simulate_keystrokes("f19");
    }
    assert!(ringed(cx), "focused from the keyboard, it is drawn with its ring");
}

/// Open `key`'s menu from its row in the navigator, as the pointer does.
fn machine_menu(cx: &mut VisualTestContext, key: WorkerKey) {
    hover(cx, leak(format!("nav-worker-{key}")));
    click(cx, leak(format!("nav-machine-menu-{key}")));
}

/// A machine's "…" in the navigator says what it runs and does what the app lets it: its
/// system, then each agent installed there with its version, then Connect only while its link
/// is down, and Forget. Each runs the app's own and closes the menu. No readout counts
/// machines.
#[gpui::test]
fn a_machines_menu_says_what_it_runs_and_does_what_the_app_lets_it(cx: &mut TestAppContext) {
    use slopty_proto::server::{InstalledAgent, Os, WorkerCaps};
    use slopty_proto::thread::AgentId;

    let (view, cx) = workspace(cx);
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let (studio_key, laptop_key) = (studio.key, laptop.key);
    let (connected, forgot) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
    view.update_in(cx, |v, _w, cx| {
        let agent = |id: &str, version: &str| InstalledAgent {
            agent: AgentId(id.to_owned()),
            version: version.to_owned(),
            offers: slopty_proto::thread::Offers::default(),
        };
        let caps = WorkerCaps {
            os_version: "26.5".to_owned(),
            agents: vec![
                agent(AgentId::CLAUDE_CODE, "2.1.3 (Claude Code)"),
                agent(AgentId::CODEX, "codex-cli 0.48.0"),
            ],
            ..WorkerCaps::bare(Os::MacOs)
        };
        v.set_worker_caps(studio_key, caps, cx);
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
                    ..HostActions::default()
                };
                (k, actions)
            })
            .collect();
        v.set_host_actions(hosts, None, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-workers").is_none(), "the bar counts no machines");

    machine_menu(cx, studio_key);
    let facts = view.read_with(cx, |v, _| v.machine_facts(studio_key));
    assert_eq!(facts, ["macOS 26.5 · load 2.1", "Claude Code 2.1.3", "Codex 0.48.0"]);
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "Claude Code 2.1.3"), "read-only lines: {names:#?}");
    assert!(cx.debug_bounds("menu-Connect").is_none(), "connected: no Connect");
    assert!(cx.debug_bounds("menu-Forget").is_some());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    machine_menu(cx, laptop_key);
    click(cx, "menu-Connect");
    assert!(connected.get(), "the app dials it");
    assert!(cx.debug_bounds("menu").is_none(), "and the menu goes");
    machine_menu(cx, laptop_key);
    click(cx, "menu-Forget");
    assert!(forgot.get());
}

/// How the tailnet carries a link is said only for a DERP relay, the slow path, in the
/// navigator: a direct or peer-relayed link is quiet everywhere. It goes with the link, and a
/// link the worker has said nothing of shows none.
#[gpui::test]
fn the_link_path_shows_beside_the_round_trip_and_goes_with_the_link(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let lan = connect(&view, cx, 3, "lan");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let (studio_key, laptop_key) = (studio.key, laptop.key);
    let _lan = lan;
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
    assert!(cx.debug_bounds("readouts").is_none(), "no readout says a path");
    assert!(cx.debug_bounds(leak(format!("nav-path-{studio_key}"))).is_none(), "direct is quiet");
    assert!(cx.debug_bounds(leak(format!("nav-path-{laptop_key}"))).is_none());

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
/// found. Then the title bar's readouts say it in words for the focused machine, in the muted
/// tone, with the fix under the pointer, and the navigator names the relay. A direct path takes
/// both away.
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
    assert!(cx.debug_bounds("readout-relay").is_none(), "a path still settling says nothing");
    assert!(cx.debug_bounds(nav).is_none());

    let almost = slopty_proto::tailnet::DERP_NOTICE_AFTER.saturating_sub(Duration::from_secs(1));
    cx.executor().advance_clock(almost);
    // The worker says the path again: the relay's clock is not restarted by it.
    view.update_in(cx, |v, _w, cx| v.set_link_path(key, derp(), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("readout-relay").is_none(), "nine seconds are not enough");
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
    assert!(cx.debug_bounds("readout-relay").is_none(), "a direct path takes it away");
    assert!(cx.debug_bounds(nav).is_none());
}

/// A worker the app can wake gets "Wake <name>" among the palette's commands and a Wake in its
/// row's menu, and either runs the app's wake; a worker the app cannot wake gets neither.
#[gpui::test]
fn a_sleeping_worker_is_woken_from_the_palette_and_its_row(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.update(|_w, cx| cx.set_reduce_motion(true));
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

    machine_menu(cx, studio_key);
    assert!(cx.debug_bounds("menu-Wake").is_none());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    machine_menu(cx, laptop_key);
    click(cx, "menu-Wake");
    assert_eq!(woke.get(), 2, "and so did the row's menu");
}

/// The palette, or the machine's menu on its row, stops sharing the clipboard with
/// one machine and shares it again, by its name: the navigator marks the machine it is not
/// shared with, the app is told to keep the choice, and the other machine is left as it was.
#[gpui::test]
fn the_clipboard_is_stopped_and_shared_with_one_machine_from_the_palette_or_its_row(
    cx: &mut TestAppContext,
) {
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

    // The machine's menu says the same.
    machine_menu(cx, studio_key);
    click(cx, "menu-Unshare clipboard");
    assert!(!view.read_with(cx, |v, _| v.clipboard_shared(studio_key)));
    assert!(cx.debug_bounds("menu").is_none(), "and the menu goes");
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("The clipboard is no longer shared with studio")
    );
    machine_menu(cx, studio_key);
    click(cx, "menu-Share clipboard");
    assert!(view.read_with(cx, |v, _| v.clipboard_shared(studio_key)));
    assert_eq!(
        events.borrow().as_slice(),
        [
            WorkspaceEvent::ClipboardShared { worker: laptop_key, share: false },
            WorkspaceEvent::ClipboardShared { worker: laptop_key, share: true },
            WorkspaceEvent::ClipboardShared { worker: studio_key, share: false },
            WorkspaceEvent::ClipboardShared { worker: studio_key, share: true },
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
    assert!(names.iter().any(|l| l == "studio, not granted"), "the navigator: {names:#?}");
    assert_eq!(WorkerStatus::NotGranted.text(), "closed to this device by the tailnet policy");
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
/// earns its tile's badge, as the note that went out for it says, but no count on the bell,
/// which speaks for agents. With the app in front, the same end on the focused tile is seen as
/// it happens and leaves nothing.
#[gpui::test]
fn a_command_ending_on_the_focused_tile_while_away_is_badged(cx: &mut TestAppContext) {
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
        assert!(v.finished(seen).is_none(), "watched in front: no badge");
        assert!(v.finished(missed).is_some(), "on the focused tile, but the app was away");
        assert_eq!(v.bell_count(), 0, "a shell's finish is not the bell's");
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

/// The title bar says the focused tile's machine's plan windows as its agents published them
/// once one is 80 % used, and a click lists every machine's readings. Under that it says
/// nothing, and a machine whose agents published none shows no meter.
#[gpui::test]
fn a_far_used_plan_is_said_in_the_title_bar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let here = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let there = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 1);
    let key = studio.key;
    let publish = |cx: &mut VisualTestContext, seq, seven_day| {
        let row = plan_row(vec![window("five-hour", 2_300), window("seven-day", seven_day)]);
        let table = slopty_proto::thread::wire::TableFrame::Snapshot {
            cursor: slopty_proto::thread::Cursor { epoch: 1, seq },
            rows: vec![row],
        };
        view.update_in(cx, |v, _w, cx| {
            v.threads_linked(key, cx);
            v.thread_table(key, &table, cx);
            v.focus_tile(here, cx);
        });
        cx.run_until_parked();
    };
    publish(cx, 1, 4_100);
    assert!(cx.debug_bounds("readout-plan").is_none(), "41 % is no news");
    publish(cx, 2, 8_200);
    assert!(labels(&view, cx).iter().any(|l| l == "Plan usage 5h 23% · 7d 82%"));

    click(cx, "readout-plan");
    assert!(cx.debug_bounds("plans").is_some(), "every reading, listed");
    let row = "studio · Claude Code, 5h 23% · 7d 82% · now".to_owned();
    assert!(labels(&view, cx).contains(&row), "{:?}", labels(&view, cx));
    let (bar, plans) = (cx.debug_bounds("titlebar"), cx.debug_bounds("plans"));
    assert!(bar.zip(plans).is_some_and(|(b, p)| p.top() >= b.bottom()), "under the title bar");

    view.update_in(cx, |v, _w, cx| v.focus_tile(there, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("readout-plan").is_none(), "the laptop's agents said nothing");
}

/// A worker on another build shows Update where it is named, not only on its tiles: on its
/// navigator row, at rest. It runs the app's update against the host the notice names; with one
/// under way, it is not offered again.
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

/// The start page names a worker on another build with its Update too: an empty workspace is
/// what a morning after an app update may open on, and its row opens nothing until then.
#[gpui::test]
fn the_start_page_offers_a_worker_on_another_build_its_update(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let asked: Rc<std::cell::RefCell<Vec<String>>> = Rc::default();
    let seen = Rc::clone(&asked);
    cx.update(|_w, cx| {
        let start: crate::add_worker::Update =
            Rc::new(move |host: &str, _w, _cx| seen.borrow_mut().push(host.to_owned()));
        cx.set_global(crate::add_worker::Updates { start: Some(start), ..Default::default() });
    });
    assert!(cx.debug_bounds("empty-update-0").is_none(), "nothing to update while linked");
    let notice =
        UpdateNotice { of: Of::Worker, host: "studio.ts.net".to_owned(), peer: "0.0.9".to_owned() };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.disconnect_worker(key, WorkerStatus::NeedsUpdate(notice), cx));
    cx.run_until_parked();
    click(cx, "empty-update-0");
    assert_eq!(*asked.borrow(), ["studio.ts.net"], "the start page's Update");
}

/// The title bar's empty span moves the window as a native title bar does: pressed and moved,
/// it asks the system to drag the window; a double-click asks for the system's title-bar
/// action (zoom or minimise, as the person set it). A press on one of its buttons does neither.
#[gpui::test]
fn the_title_bars_empty_span_moves_and_zooms_the_window(cx: &mut TestAppContext) {
    use crate::workspace::titlebar::WindowAsk;
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let bar = cx.debug_bounds("titlebar").expect("the bar");
    let left = MouseButton::Left;
    let asks = |cx: &mut VisualTestContext| view.update(cx, |v, _| v.take_window_asks());
    let empty = point(bar.center().x, bar.center().y);

    cx.simulate_mouse_down(empty, left, Modifiers::default());
    cx.simulate_mouse_move(point(empty.x + px(4.0), empty.y), Some(left), Modifiers::default());
    cx.simulate_mouse_up(point(empty.x + px(4.0), empty.y), left, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(asks(cx), [WindowAsk::Move], "pressed and moved, the window follows");

    cx.simulate_mouse_move(empty, None, Modifiers::default());
    cx.run_until_parked();
    assert!(asks(cx).is_empty(), "a move with no press does nothing");

    let modifiers = Modifiers::default();
    cx.simulate_event(gpui::MouseDownEvent {
        button: left,
        position: empty,
        modifiers,
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        button: left,
        position: empty,
        modifiers,
        click_count: 2,
    });
    cx.run_until_parked();
    assert_eq!(asks(cx), [WindowAsk::TitleBarDoubleClick], "a double-click zooms it");

    let more = cx.debug_bounds("more").expect("the menu button").center();
    cx.simulate_mouse_down(more, left, Modifiers::default());
    cx.simulate_mouse_move(point(more.x - px(6.0), more.y), Some(left), Modifiers::default());
    cx.simulate_mouse_up(point(more.x - px(6.0), more.y), left, Modifiers::default());
    cx.run_until_parked();
    assert!(asks(cx).is_empty(), "a button's press is its own");
}

/// "Edit <machine>'s settings" opens that machine's own `settings.toml`, where its greeting
/// said it is, in a file tile on it, from the palette and from its menu; a machine that named
/// no file offers neither.
#[gpui::test]
fn a_machines_settings_open_in_a_file_tile_on_it(cx: &mut TestAppContext) {
    const PATH: &str = "/Users/me/Library/Application Support/Slopty/settings.toml";
    let (view, cx) = workspace(cx);
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    let (tx, rx) = mpsc::channel(256);
    let key = WorkerKey::new(1);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    view.update_in(cx, |v, _window, cx| {
        v.add_worker(key, "studio".to_owned(), cx);
        let me = ClientId::new();
        let ack = HelloAck { settings: PATH.to_owned(), ..hello("studio", Vec::new()) };
        v.connect_worker(
            key,
            WorkerLink { me, out: tx, open_screen: factory, remote: None },
            ack,
            cx,
        );
        v.apply_sync(key, ItemSync::Snapshot { version: 0, items: Vec::new() }, cx);
    });
    cx.run_until_parked();
    let mut studio = Fake { key, me: ClientId::new(), rx };
    studio.drain();
    let laptop = connect(&view, cx, 2, "laptop");

    let lines: Vec<String> = view.update(cx, |v, cx| {
        v.palette_lines(cx)
            .into_iter()
            .map(|l| l.label)
            .filter(|l| l.ends_with("settings"))
            .collect()
    });
    assert_eq!(lines, ["Edit studio's settings"], "only a machine that said where");

    let opened = |studio: &mut Fake| {
        studio.drain().into_iter().find_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(Item { kind: ItemKind::File { path }, .. })) => Some(path),
            _ => None,
        })
    };
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_input("Edit studio's settings");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(opened(&mut studio).as_deref(), Some(PATH), "a file tile on the studio");

    machine_menu(cx, laptop.key);
    assert!(cx.debug_bounds("menu-Edit settings").is_none(), "the laptop named no file");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    machine_menu(cx, key);
    click(cx, "menu-Edit settings");
    assert!(cx.debug_bounds("menu").is_none(), "and the menu goes");
    let focused = view.read_with(cx, |v, _| {
        v.focused().and_then(|t| v.item(t)).map(|i| (v.focused().map(|t| t.worker), i.kind.clone()))
    });
    let file = ItemKind::File { path: PATH.to_owned() };
    assert_eq!(focused, Some((Some(key), file)), "the open tile, focused, not a second one");
    assert_eq!(opened(&mut studio), None, "no second tile");
}

/// "Remove…" on a machine's menu, and "Remove studio…" in the palette, ask first: the confirm
/// says what goes and that the person's repositories, worktrees and agent sessions stay, and
/// that the shell open there ends. Cancel, Esc and a click outside remove nothing; Remove runs
/// the app's removal once. For this Mac it adds that Slopty no longer opens at login, and a
/// machine the app cannot remove offers neither line.
#[gpui::test]
fn removing_a_machine_asks_first_and_says_what_stays(cx: &mut TestAppContext) {
    use super::super::actions::RemoveMachine;
    use super::super::machine_remove::{NO_LOGIN, REMOVE};

    let (view, cx) = workspace(cx);
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let (studio_key, laptop_key) = (studio.key, laptop.key);
    let removed = Rc::new(Cell::new(0_u32));
    let set = |here: bool, cx: &mut VisualTestContext| {
        let counted = Rc::clone(&removed);
        let remove: MenuRun = Rc::new(move |_w, _cx| counted.set(counted.get().saturating_add(1)));
        let hosts = [
            (studio_key, HostActions { remove: Some(remove), here, ..HostActions::default() }),
            (laptop_key, HostActions::default()),
        ]
        .into_iter()
        .collect();
        view.update_in(cx, |v, _w, cx| v.set_host_actions(hosts, None, cx));
        cx.run_until_parked();
    };
    set(false, cx);
    let lines: Vec<String> =
        view.update(cx, |v, cx| v.palette_lines(cx)).into_iter().map(|l| l.label).collect();
    assert!(lines.iter().any(|l| l == "Remove studio\u{2026}"), "{lines:?}");
    assert!(!lines.iter().any(|l| l == "Remove laptop\u{2026}"), "nothing the app cannot remove");
    let words =
        |cx: &mut VisualTestContext| view.read_with(cx, WorkspaceView::remove_machine_words);

    machine_menu(cx, studio_key);
    click(cx, leak(format!("menu-{REMOVE}")));
    let said = words(cx).expect("the confirm, before anything is removed");
    assert_eq!(said.title, "Remove studio?");
    assert!(
        said.body.ends_with(
            "Your repositories, worktrees and agent sessions on studio stay as they are."
        ),
        "{}",
        said.body
    );
    assert_eq!(said.also, ["Its one open shell or agent ends."]);
    cx.update(|window, _| window.set_a11y_active(true));
    cx.run_until_parked();
    let tree = cx.update(|window, _| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("AlertDialog", Some("Remove studio?"))), "an alert");
    click(cx, "remove-machine-cancel");
    assert!(words(cx).is_none() && removed.get() == 0, "Cancel removes nothing");

    view.update_in(cx, |v, w, cx| v.remove_machine(&RemoveMachine { worker: studio_key }, w, cx));
    cx.run_until_parked();
    assert!(words(cx).is_some(), "the palette's line asks too");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(words(cx).is_none() && removed.get() == 0, "Esc removes nothing");

    view.update_in(cx, |v, w, cx| v.remove_machine(&RemoveMachine { worker: studio_key }, w, cx));
    cx.run_until_parked();
    click(cx, "remove-machine-confirm");
    assert_eq!(removed.get(), 1, "Remove runs the app's removal");
    assert!(words(cx).is_none(), "and the confirm goes");

    set(true, cx);
    view.update_in(cx, |v, w, cx| v.remove_machine(&RemoveMachine { worker: studio_key }, w, cx));
    cx.run_until_parked();
    let said = words(cx).expect("asked");
    assert_eq!(said.also.last().map(String::as_str), Some(NO_LOGIN), "this Mac: {said:?}");
    view.update_in(cx, |v, w, cx| v.remove_machine(&RemoveMachine { worker: laptop_key }, w, cx));
    assert_eq!(words(cx).map(|w| w.title).as_deref(), Some("Remove studio?"), "laptop: none");
}

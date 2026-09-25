//! A tile's chrome in the headless workspace: what its header says (kind, place, worker,
//! status), the surface it sits on, the controls it offers, the pill over a body that cannot
//! show what it should, and the notices in the strip's corner.

use slopty_grid::SemanticMark;

use super::*;
use crate::workspace::tile::{CLOSE_TILE, RECONNECTING, cwd_tail};
use crate::workspace::toast::{SAY_FOR, SHOWN};

/// The accessibility tree of the next frame.
fn tree(cx: &mut VisualTestContext) -> Vec<crate::a11y::Node> {
    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.run_until_parked();
    cx.update(|window, _cx| crate::a11y::tree(window))
}

/// The status marks drawn, by label.
fn marks(cx: &mut VisualTestContext) -> Vec<String> {
    tree(cx).into_iter().filter(|n| n.role == "Image").filter_map(|n| n.label).collect()
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn")).center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

#[test]
fn a_place_is_its_last_two_directories_with_home_as_a_tilde() {
    assert_eq!(cwd_tail("/Users/w"), "~");
    assert_eq!(cwd_tail("/Users/w/"), "~");
    assert_eq!(cwd_tail("/Users/w/src"), "~/src");
    assert_eq!(cwd_tail("/Users/w/Workspace/oss/slopty"), "oss/slopty");
    assert_eq!(cwd_tail("/home/w/src"), "~/src");
    assert_eq!(cwd_tail("/root"), "~");
    assert_eq!(cwd_tail("/usr/local/bin"), "local/bin");
    assert_eq!(cwd_tail("/etc"), "/etc");
    assert_eq!(cwd_tail("/Users"), "/Users", "the folder of homes is not a home");
    assert_eq!(cwd_tail("/"), "/");
}

/// A shell's header names where it is; a note, which is nowhere, names no place.
#[gpui::test]
fn a_shell_header_names_its_directory(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shell =
        opens_in(&view, cx, &fake, SessionId::new(), fake.me, 1, Some("/Users/w/src/slopty"));
    let note = arrives(&view, cx, &fake, ItemKind::Note { text: "plan".into() }, 2);
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Label", Some("src/slopty"))), "{nodes:#?}");
    assert!(cx.debug_bounds(selector("place", shell.item)).is_some());
    assert!(cx.debug_bounds(selector("place", note.item)).is_none(), "a note has no place");
}

/// The worker's name is worth a chip only when there is another worker it could be.
#[gpui::test]
fn the_worker_chip_shows_only_beside_another_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("worker", shell.item)).is_none(), "one worker: no chip");
    let _laptop = connect(&view, cx, 2, "laptop");
    let nodes = tree(cx);
    assert!(cx.debug_bounds(selector("worker", shell.item)).is_some(), "two: the chip");
    assert!(nodes.iter().any(|n| n.is("Label", Some("studio"))), "{nodes:#?}");
}

/// The focused tile's header is its body's surface; another tile's steps up to the panel.
#[gpui::test]
fn a_focused_header_shares_its_body_surface(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let second = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    assert_eq!(focused(&view, cx), Some(second));
    cx.run_until_parked();
    let panel = gpui::Background::from(crate::colors::hsla(Theme::default().surfaces.panel));
    let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
    let first_header = cx.debug_bounds(selector("title", first.item)).expect("drawn");
    let second_header = cx.debug_bounds(selector("title", second.item)).expect("drawn");
    let on_panel = |header: Bounds<Pixels>| {
        let near = |a: f32, b: Pixels| f32::from(b).mul_add(-scale, a).abs() < 1.0;
        quads.iter().any(|q| {
            near(q.bounds.origin.x.0, header.origin.x)
                && near(q.bounds.origin.y.0, header.origin.y)
                && near(q.bounds.size.width.0, header.size.width)
                && near(q.bounds.size.height.0, header.size.height)
                && q.background == panel
        })
    };
    assert!(on_panel(first_header), "an unfocused header sits on the panel");
    assert!(!on_panel(second_header), "the focused one is its body's surface");
}

/// One mark says how each tile is doing: its agent's state, a shell's failed last command,
/// and a worker out of reach.
#[gpui::test]
fn the_status_mark_follows_the_agent_the_last_exit_and_the_link(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let (agent, shell) = (SessionId::new(), SessionId::new());
    opens(&view, cx, &fake, agent, fake.me, 1);
    opens(&view, cx, &fake, shell, fake.me, 2);
    assert!(marks(cx).iter().all(|m| m != "Needs you" && m != "Failed"), "nothing to say yet");

    view.update_in(cx, |v, _w, cx| {
        v.agent_event(
            AgentEvent {
                session: agent,
                kind: AgentKind::ClaudeCode,
                status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
                agent_session: None,
                detail: None,
                attention: false,
                source: AgentSource::Hook,
            },
            cx,
        );
        let prompt = |exit| SemanticMark::Prompt { exit, input: Some(2) };
        let rows = [("$ false", prompt(None)), ("$ ", prompt(Some(1)))];
        v.term_event(shell, marked_frame(1, &rows, 1), cx);
    });
    let drawn = marks(cx);
    assert!(drawn.iter().any(|m| m == "Needs you"), "the agent waits: {drawn:?}");
    assert!(drawn.iter().any(|m| m == "Failed"), "the shell's last command failed: {drawn:?}");

    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    let drawn = marks(cx);
    assert_eq!(drawn.iter().filter(|m| *m == "Away").count(), 2, "both are away: {drawn:?}");
}

/// Close and fullscreen on the header do what ⌘W and ⌃⌘F do to their tile.
#[gpui::test]
fn the_header_controls_close_and_fullscreen_their_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let _first = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let second = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    let width = |cx: &mut VisualTestContext| {
        f32::from(cx.debug_bounds(selector("item", second.item)).expect("drawn").size.width)
    };
    let before = width(cx);
    click(cx, selector("fullscreen", second.item));
    assert!(width(cx) > before + 100.0, "fullscreen: {before} → {}", width(cx));

    fake.drain();
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Button", Some(CLOSE_TILE))), "{nodes:#?}");
    click(cx, selector("close", second.item));
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Remove(i)) if *i == second.item)),
        "{sent:?}"
    );
    assert!(cx.debug_bounds("closed").is_some(), "and it can be taken back");
}

/// A tile whose worker dropped says so in a pill at the foot of its body, not in a dialog.
#[gpui::test]
fn a_tile_whose_worker_dropped_says_reconnecting_at_its_foot(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("state", tile.item)).is_none(), "linked: nothing to say");
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Status", Some(RECONNECTING))), "{nodes:#?}");
    let pill = cx.debug_bounds(selector("state", tile.item)).expect("the pill is drawn");
    let body = cx.debug_bounds(selector("item", tile.item)).expect("the tile is drawn");
    assert!((pill.center().x - body.center().x).abs() < px(1.0), "centred");
    assert!(pill.center().y > body.center().y && pill.bottom() < body.bottom(), "at the foot");
}

/// A shell whose program exited says how, and offers to start it again where it was or to
/// close it.
#[gpui::test]
fn an_exited_shell_offers_restart_and_close(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::Terminal { session },
        sleeping: false,
        name: None,
    };
    let tile = TileRef { worker: fake.key, item: item.id };
    let (key, me) = (fake.key, fake.me);
    view.update_in(cx, |v, _w, cx| {
        let exited = SessionSummary {
            state: SessionState::Exited { status: 2 },
            command: vec!["make".into()],
            ..summary(session, Some("/w/src"))
        };
        v.session_opened(key, exited, cx);
        v.apply_sync(key, ItemSync::Delta { version: 1, by: me, op: ItemOp::Upsert(item) }, cx);
    });
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Status", Some("Exited · code 2"))), "{nodes:#?}");
    assert!(cx.debug_bounds(selector("close-ended", tile.item)).is_some(), "Close is offered");
    fake.drain();

    click(cx, selector("restart", tile.item));
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::OpenSession(o)
            if o.cwd.as_deref() == Some("/w/src") && o.command == ["make"])),
        "the same command where it was: {sent:?}"
    );
    assert!(
        sent.iter().any(
            |m| matches!(m, ClientMsg::Term { session: s, req: TermRequest::Close } if *s == session)
        ),
        "{sent:?}"
    );
    assert!(!view.read_with(cx, |v, _| v.layout().contains(tile)), "the old tile goes");
}

/// Notices stack in the strip's bottom-right corner, no wider than 400 pt, two at most (a
/// third pushes the oldest out), and each goes after six seconds.
#[gpui::test]
fn notices_stack_two_in_the_corner_and_go(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _fake = connect(&view, cx, 1, "studio");
    let long = "a word that goes on ".repeat(10);
    for text in ["one", "two", long.as_str()] {
        view.update_in(cx, |v, _w, cx| v.show_notice(text.to_owned(), cx));
        cx.executor().advance_clock(Duration::from_secs(1));
    }
    cx.run_until_parked();
    let shown = view.read_with(cx, WorkspaceView::toast_texts);
    assert_eq!(shown.len(), SHOWN);
    assert_eq!(shown, vec!["two".to_owned(), long.clone()], "the oldest went");

    let nodes = tree(cx);
    let [x, y, w, _] = nodes
        .iter()
        .find(|n| n.is("Status", Some(long.as_str())))
        .map_or_else(|| panic!("the long notice is drawn: {nodes:#?}"), |n| n.bounds);
    let strip = cx.debug_bounds("workspace").expect("drawn");
    assert!(w <= 400.0, "capped at 400 pt: {w}");
    assert!(x > f32::from(strip.center().x), "on the right: {x}");
    assert!(y > f32::from(strip.center().y), "at the foot: {y}");

    cx.executor().advance_clock(SAY_FOR);
    cx.run_until_parked();
    assert!(view.read_with(cx, WorkspaceView::toast_texts).is_empty(), "gone after 6 s");
}

/// The closed tile's notice offers it back, and its Undo does what ⌘Z does.
#[gpui::test]
fn the_closed_notice_takes_the_tile_back(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let [_, _, (_, third)] = three_shells(&view, cx, &fake);
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.layout().contains(third)), "off the strip");
    fake.drain();
    click(cx, "toast-undo");
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Upsert(i)) if i.id == third.item)),
        "{sent:?}"
    );
    assert!(view.read_with(cx, |v, _| v.layout().contains(third)), "back");
    assert!(cx.debug_bounds("closed").is_none(), "the offer is taken");
}

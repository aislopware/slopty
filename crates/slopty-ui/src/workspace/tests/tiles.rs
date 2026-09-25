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

/// A narrow tile keeps its name whole and lets where it is give way.
#[gpui::test]
fn a_narrow_header_keeps_its_title_and_shortens_its_place(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(720.0), px(600.0)));
    let fake = connect(&view, cx, 1, "studio");
    let named = |session| Item {
        id: ItemId::new(),
        kind: ItemKind::Terminal { session },
        sleeping: false,
        name: Some("release notes".to_owned()),
    };
    let (here, nowhere) = (SessionId::new(), SessionId::new());
    let (placed, bare) = (named(here), named(nowhere));
    let (placed_id, bare_id) = (placed.id, bare.id);
    let key = fake.key;
    view.update_in(cx, |v, _window, cx| {
        v.session_opened(key, summary(here, Some("/srv/deployments/production-eu-west")), cx);
        v.session_opened(key, summary(nowhere, None), cx);
        for (version, item) in [(1, placed), (2, bare)] {
            let op = ItemOp::Upsert(item);
            v.apply_sync(key, ItemSync::Delta { version, by: fake.me, op }, cx);
        }
    });
    cx.run_until_parked();
    let width = |cx: &mut VisualTestContext, what: &str, item: ItemId| {
        let b = cx.debug_bounds(selector(what, item)).unwrap_or_else(|| panic!("{what} drawn"));
        f32::from(b.size.width)
    };
    let (title, alone) = (width(cx, "name", placed_id), width(cx, "name", bare_id));
    assert!((title - alone).abs() < 1.0, "the title is whole beside its place: {title} vs {alone}");
    let place = width(cx, "place", placed_id);
    assert!(place > 0.0 && place < 100.0, "the place gave way: {place}");
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

/// The quads painted at `bounds` (points), as the window drew them (device pixels).
fn quads_at(cx: &mut VisualTestContext, bounds: Bounds<Pixels>) -> Vec<gpui::Quad> {
    let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
    let near = |a: f32, b: Pixels| f32::from(b).mul_add(-scale, a).abs() < 1.0;
    quads
        .into_iter()
        .filter(|q| {
            near(q.bounds.origin.x.0, bounds.origin.x)
                && near(q.bounds.origin.y.0, bounds.origin.y)
                && near(q.bounds.size.width.0, bounds.size.width)
                && near(q.bounds.size.height.0, bounds.size.height)
        })
        .collect()
}

/// Panes sit flush: no tile is rounded or framed, the focused one included, and every header
/// lies on its body's surface with one hairline under it.
#[gpui::test]
fn tiles_have_no_frame_or_corner_and_headers_sit_on_their_bodies(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let second = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    assert_eq!(focused(&view, cx), Some(second));
    cx.run_until_parked();
    let panel = gpui::Background::from(crate::colors::hsla(Theme::default().surfaces.panel));
    for tile in [first, second] {
        let bounds = cx.debug_bounds(selector("item", tile.item)).expect("drawn");
        let quads = quads_at(cx, bounds);
        assert!(!quads.is_empty(), "the tile paints its surface");
        for q in &quads {
            let r = q.corner_radii;
            let w = q.border_widths;
            assert!(
                [r.top_left, r.top_right, r.bottom_left, r.bottom_right].iter().all(|c| c.0 == 0.0),
                "a square tile: {q:?}"
            );
            assert!(
                [w.top, w.right, w.bottom, w.left].iter().all(|b| b.0 == 0.0),
                "no frame: {q:?}"
            );
        }
        let header = cx.debug_bounds(selector("title", tile.item)).expect("drawn");
        let quads = quads_at(cx, header);
        assert!(quads.iter().all(|q| q.background != panel), "on the body's surface");
        assert!(quads.iter().any(|q| q.border_widths.bottom.0 > 0.0), "a hairline under it");
    }
}

/// One divider between each pair of neighbours: a tile draws it on its right edge where a
/// column follows and on its bottom edge where a tile is stacked below, and nowhere else.
#[gpui::test]
fn a_divider_runs_only_between_neighbours(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), (_, second), (_, third)] = three_shells(&view, cx, &fake);
    cx.simulate_keystrokes("cmd-[");
    cx.run_until_parked();
    assert_eq!(column_of(&view, cx, third), column_of(&view, cx, second), "stacked");
    let drawn = |cx: &mut VisualTestContext, part: &str, tile: TileRef| {
        cx.debug_bounds(selector(part, tile.item)).is_some()
    };
    assert!(drawn(cx, "divider-right", first), "a column follows the first");
    assert!(!drawn(cx, "divider-below", first));
    assert!(!drawn(cx, "divider-right", second), "the last column: nothing to its right");
    assert!(drawn(cx, "divider-below", second), "the third is below it");
    assert!(!drawn(cx, "divider-right", third) && !drawn(cx, "divider-below", third));

    let border = gpui::Background::from(crate::colors::hsla(Theme::default().surfaces.border));
    let line = cx.debug_bounds(selector("divider-below", second.item)).expect("drawn");
    assert!((f32::from(line.size.height) - 1.0).abs() < 0.01, "a hairline");
    assert!(quads_at(cx, line).iter().any(|q| q.background == border), "in the border colour");
}

/// The focused tile is left alone and the others lie under the canvas colour, a quad over
/// the body and not an opacity; a tile alone in view is never veiled.
#[gpui::test]
fn unfocused_bodies_are_veiled_and_a_lone_tile_is_not(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let lone = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    assert!(cx.debug_bounds(selector("veil", lone.item)).is_none(), "alone: no veil");
    let second = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    view.update_in(cx, |v, _w, cx| v.focus_tile(lone, cx));
    cx.run_until_parked();
    let veil = cx.debug_bounds(selector("veil", second.item)).expect("the other tile is veiled");
    let body = cx.debug_bounds(selector("title", second.item)).expect("drawn");
    assert!(veil.top() >= body.bottom(), "over the body, not the header");
    let theme = Theme::default();
    let wash = gpui::Background::from(crate::colors::hsla_alpha(
        theme.surfaces.canvas,
        slopty_theme::alpha::FAINT,
    ));
    assert!(quads_at(cx, veil).iter().any(|q| q.background == wash), "canvas at FAINT");
    assert!(cx.debug_bounds(selector("veil", lone.item)).is_none(), "the focused one is not");

    view.update_in(cx, |v, _w, cx| v.focus_tile(second, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("veil", lone.item)).is_some(), "the veil follows the focus");
    assert!(cx.debug_bounds(selector("veil", second.item)).is_none());
}

/// A tile whose agent waits on the human carries a warn bar along the top of its header, and
/// no outline.
#[gpui::test]
fn a_tile_that_needs_you_has_a_warn_bar_on_its_header(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let agent = SessionId::new();
    let waiting = opens(&view, cx, &fake, agent, fake.me, 1);
    let other = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    assert!(cx.debug_bounds(selector("attention", waiting.item)).is_none(), "not yet");
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
    });
    cx.run_until_parked();
    let bar = cx.debug_bounds(selector("attention", waiting.item)).expect("the bar");
    let header = cx.debug_bounds(selector("title", waiting.item)).expect("drawn");
    assert_eq!(bar.top(), header.top(), "along the top");
    assert_eq!(bar.size.width, header.size.width);
    assert!((f32::from(bar.size.height) - 2.0).abs() < 0.01, "2 pt");
    let warn = gpui::Background::from(crate::colors::hsla(Theme::default().surfaces.warn));
    assert!(quads_at(cx, bar).iter().any(|q| q.background == warn));
    assert!(cx.debug_bounds(selector("attention", other.item)).is_none(), "only on that one");
    let tile = cx.debug_bounds(selector("item", waiting.item)).expect("drawn");
    assert!(quads_at(cx, tile).iter().all(|q| q.border_widths.left.0 == 0.0), "no outline");
}

/// One mark says how each tile is doing: its agent's state, a shell's failed last command,
/// and a worker out of reach.
#[gpui::test]
fn the_status_mark_follows_the_agent_the_last_exit_and_the_link(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let (agent, shell) = (SessionId::new(), SessionId::new());
    let agent_tile = opens(&view, cx, &fake, agent, fake.me, 1);
    let shell_tile = opens(&view, cx, &fake, shell, fake.me, 2);
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
    let nodes = tree(cx);
    let headers: Vec<Bounds<Pixels>> = [agent_tile, shell_tile]
        .iter()
        .filter_map(|t| cx.debug_bounds(selector("title", t.item)))
        .collect();
    let in_a_header = |n: &crate::a11y::Node| {
        let [x, y, ..] = n.bounds;
        headers.iter().any(|h| h.contains(&point(px(x), px(y))))
    };
    let away = nodes.iter().filter(|n| n.is("Image", Some("Away")) && in_a_header(n)).count();
    assert_eq!(away, 2, "both headers say away: {nodes:#?}");
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

/// A shell whose program exits keeps its tile, its last screen and the pill until the human
/// closes it: nothing is sent to the worker until then, and Close then takes the tile off and
/// the session after the undo window, as ⌘W does.
#[gpui::test]
fn an_exited_shell_stays_until_it_is_closed(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, fake.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.term_event(session, frame(&["$ exit 1"]), cx);
        v.term_event(session, TermEvent::Exited { status: 1 }, cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(UNDO_CLOSE);
    cx.run_until_parked();
    let closes = |sent: &[ClientMsg]| {
        sent.iter()
            .filter(|m| matches!(m, ClientMsg::Term { session: s, req: TermRequest::Close } if *s == session))
            .count()
    };
    assert_eq!(closes(&fake.drain()), 0, "the exit alone closes nothing");
    assert!(view.read_with(cx, |v, _| v.layout().contains(tile)), "the tile stays");
    assert!(view.read_with(cx, |v, _| v.terminal(session).is_some()), "with its screen");
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Status", Some("Exited · code 1"))), "{nodes:#?}");
    assert!(cx.debug_bounds(selector("restart", tile.item)).is_some(), "Restart is offered");

    click(cx, selector("close-ended", tile.item));
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Remove(i)) if *i == tile.item)),
        "Close takes the tile off at once: {sent:?}"
    );
    assert!(cx.debug_bounds("close-confirm").is_none(), "an exited shell does not ask");
    cx.executor().advance_clock(UNDO_CLOSE);
    cx.run_until_parked();
    assert_eq!(closes(&fake.drain()), 1, "and the worker closes the session after the undo");
}

/// A shell scrolled up into its history shows how many lines are below at the body's foot;
/// once its program exits, the tile's `Exited` pill takes that place and the count goes.
#[gpui::test]
fn the_exited_pill_takes_the_place_of_the_lines_below(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, fake.me, 1);
    let TermEvent::Frame(screen) = frame(&["$ make", "built", "$"]) else { panic!("a frame") };
    let with_history = Frame { first_visible_line: LineIndex(40), total_lines: 43, ..screen };
    view.update_in(cx, |v, _w, cx| {
        v.term_event(session, TermEvent::Frame(with_history), cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, session));
    cx.simulate_keystrokes("shift-pageup");
    cx.run_until_parked();
    assert!(cx.debug_bounds("lines-below").is_some(), "scrolled up: the count shows");
    assert!(cx.debug_bounds(selector("state", tile.item)).is_none());

    view.update_in(cx, |v, _w, cx| v.term_event(session, TermEvent::Exited { status: 0 }, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("state", tile.item)).is_some(), "the tile's pill");
    assert!(cx.debug_bounds("lines-below").is_none(), "in the count's place");
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

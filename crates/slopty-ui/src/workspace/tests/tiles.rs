//! A tile's chrome in the headless workspace: what its header says (kind, place, worker,
//! status), the surface it sits on, the controls it offers, the pill over a body that cannot
//! show what it should, and the notices in the strip's corner.

use gpui::MouseButton;
use slopty_core::WallMs;
use slopty_grid::SemanticMark;
use slopty_theme::alpha;

use super::*;
use crate::workspace::tile::{
    CLOSE_TILE, COPY_COMMAND, RECONNECTING, command_words, cwd_tail, only_moves, reconnecting,
};
use crate::workspace::toast::{SAY_FOR, SHOWN};

/// The status marks drawn, by label.
fn marks(cx: &mut VisualTestContext) -> Vec<String> {
    tree(cx).into_iter().filter(|n| n.role == "Image").filter_map(|n| n.label).collect()
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn")).center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

/// A command names its shell without the `cd` it leads with, which the place already says;
/// a `cd` alone, or one that leads nowhere, is the command.
#[test]
fn a_command_is_named_without_the_cd_before_it() {
    assert_eq!(command_words("cd ~/srv/atlas && docker compose pull"), "docker compose pull");
    assert_eq!(command_words("  cd /a; cd b &&  make\nsecond line"), "make");
    assert_eq!(command_words("cd 'my dir' && ls"), "ls");
    assert_eq!(command_words(r"cd my\ dir && ls"), "ls");
    assert_eq!(command_words("cd \"a && b\" && ls"), "ls");
    assert_eq!(command_words("cd ~/srv"), "cd ~/srv");
    assert_eq!(command_words("cd ~/srv &&"), "cd ~/srv &&");
    assert_eq!(command_words("cdk deploy"), "cdk deploy");
    assert_eq!(command_words("make && cd out"), "make && cd out");
    // A `cd` alone only moves the shell: its row's place already says where.
    for moves in ["cd ~/code/atlas", "cd", "  cd 'my dir'  ", "cd /a && cd b"] {
        assert!(only_moves(moves), "{moves}");
    }
    for runs in ["cd a && ls", "cdk deploy", "cd a b", "make"] {
        assert!(!only_moves(runs), "{runs}");
    }
}

#[test]
fn a_place_is_its_last_two_directories_with_home_as_a_tilde() {
    assert_eq!(cwd_tail("/Users/w", None), "~");
    assert_eq!(cwd_tail("/Users/w/", None), "~");
    assert_eq!(cwd_tail("/Users/w/src", None), "~/src");
    assert_eq!(cwd_tail("/Users/w/Workspace/oss/slopty", None), "oss/slopty");
    assert_eq!(cwd_tail("/home/w/src", None), "~/src");
    assert_eq!(cwd_tail("/root", None), "~");
    assert_eq!(cwd_tail("/usr/local/bin", None), "local/bin");
    assert_eq!(cwd_tail("/etc", None), "/etc");
    assert_eq!(cwd_tail("/Users", None), "/Users", "the folder of homes is not a home");
    assert_eq!(cwd_tail("/", None), "/");
    // Once the worker has named its home, that and nothing else is `~`.
    let home = Some("/Volumes/Data/w");
    assert_eq!(cwd_tail("/Volumes/Data/w", home), "~");
    assert_eq!(cwd_tail("/Volumes/Data/w/src", home), "~/src");
    assert_eq!(cwd_tail("/Volumes/Data/wx/src", home), "wx/src", "a prefix is not a home");
    assert_eq!(cwd_tail("/Users/other/src", home), "other/src", "no guess by shape");
    assert_eq!(cwd_tail("/Users/w/src", Some("/Users/w/")), "~/src");
}

/// A header is its title, then its context, muted with no separator: a file's directory, a
/// shell's (none when the title already names it: the breadcrumb has the path). (That the
/// context is in the UI
/// face is `kit`'s `the_mono_face_is_for_ports_and_the_settings_file`: the test platform
/// shapes every family alike.)
#[gpui::test]
fn a_header_is_its_title_then_its_context_in_the_ui_face(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shell =
        opens_in(&view, cx, &fake, SessionId::new(), fake.me, 1, Some("/Users/w/src/slopty"));
    let main = "/Users/w/src/slopty/src/main.rs".to_owned();
    let file = arrives(&view, cx, &fake, ItemKind::File { path: main }, 2);
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Heading", Some("terminal slopty"))), "{nodes:#?}");
    assert!(nodes.iter().any(|n| n.is("Label", Some("src"))), "the file's: {nodes:#?}");
    assert!(!nodes.iter().any(|n| n.label.as_deref().is_some_and(|l| l.contains(" · "))));
    assert!(cx.debug_bounds(selector("place", file.item)).is_some(), "{file:?}");
    assert!(cx.debug_bounds(selector("place", shell.item)).is_none(), "named by its title");
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
        name: Some("release notes".to_owned()),
        facts: BTreeMap::new(),
    };
    let (here, nowhere) = (SessionId::new(), SessionId::new());
    let (placed, bare) = (named(here), named(nowhere));
    let (placed_id, bare_id) = (placed.id, bare.id);
    let key = fake.key;
    view.update_in(cx, |v, _window, cx| {
        let deep = "/srv/deployments/production-eu-west/releases/2026-10-06/artifacts/signed";
        v.session_opened(key, summary(here, Some(deep)), cx);
        v.session_opened(key, summary(nowhere, None), cx);
        for (version, item) in [(1, placed), (2, bare)] {
            let op = ItemOp::Add(item);
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
    // The path is three times what the header leaves it.
    assert!(place > 0.0 && place < 200.0, "the place gave way: {place}");
}

/// The worker's name is worth a chip only when the workspace holds another worker's tile it
/// could be confused with; the chip says the name.
#[gpui::test]
fn the_worker_chip_shows_only_beside_another_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let laptop = connect(&view, cx, 2, "laptop");
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("worker", shell.item)).is_none(), "connected is not beside");
    let _beside = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 1);
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

/// Every tile stands on a panel: the content's surface rounded at `radii.md`, its corners
/// covered with the canvas it stands on and its ring in the quiet border, the focused one
/// included. Every header lies inside its panel's top with no fill and no rule of its own:
/// focus is the title's tone and weight
/// (`focus::the_focused_tile_is_said_by_its_titles_tone_and_weight`).
#[gpui::test]
fn a_tile_stands_on_a_panel_and_its_header_on_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let second = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    assert_eq!(focused(&view, cx), Some(second));
    cx.run_until_parked();
    let theme = Theme::default();
    let scale = cx.update(|window, _| window.scale_factor());
    let content = gpui::Background::from(crate::colors::hsla(theme.content()));
    let ring = crate::colors::hsla(theme.surfaces.border_subtle);
    for tile in [first, second] {
        let bounds = cx.debug_bounds(selector("item", tile.item)).expect("drawn");
        let quads = quads_at(cx, bounds);
        let ground = quads.iter().find(|q| q.background == content).expect("its surface");
        let radius = ground.corner_radii.top_left.0 / scale;
        assert!((radius - theme.radii.md).abs() < 0.01, "rounded at radii.md: {ground:?}");
        assert!(quads.iter().any(|q| q.border_color == ring), "{tile:?}: its ring");
        let header = cx.debug_bounds(selector("title", tile.item)).expect("drawn");
        assert_eq!(header.origin, bounds.origin, "{tile:?}: the header is the panel's top");
        assert!(quads_at(cx, header).is_empty(), "{tile:?}: no band and no rule of its own");
    }
}

/// A gutter of the canvas parts every two neighbours, a column from the next and a tile from
/// the one stacked under it, and runs round the outer ones: no hairline is drawn between them.
#[gpui::test]
fn a_gutter_parts_every_neighbour(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), (_, second), (_, third)] = three_shells(&view, cx, &fake);
    cx.simulate_keystrokes("cmd-[");
    cx.run_until_parked();
    assert_eq!(column_of(&view, cx, third), column_of(&view, cx, second), "stacked");
    let at = |cx: &mut VisualTestContext, tile: TileRef| {
        cx.debug_bounds(selector("item", tile.item)).expect("drawn")
    };
    let (first, second, third) = (at(cx, first), at(cx, second), at(cx, third));
    let gutter = Theme::default().spacing.gutter();
    let near = |a: Pixels, b: f32, what: &str| {
        assert!((f32::from(a) - b).abs() < 0.5, "{what}: {a:?}, not {b}");
    };
    near(second.left() - first.right(), gutter, "between two columns");
    near(third.top() - second.bottom(), gutter, "between two stacked tiles");
    let window = cx.update(|window, _| window.viewport_size());
    near(window.height - third.bottom(), gutter, "under the last row");
    let strip = cx.debug_bounds("strip").expect("the strip");
    near(first.left() - strip.left(), gutter / 2.0, "half from the strip, half its margin");
    let border = crate::colors::hsla(Theme::default().surfaces.border);
    let lines = cx.update(|w, _| w.painted_quads());
    assert!(!lines.iter().any(|q| q.border_color == border), "no hairline parts them");
}

/// A phone's tile is full-bleed: it meets the screen's edges and its neighbours, square, with
/// no ring, as its screen shows one tile at a time.
#[gpui::test]
fn a_phone_tile_is_full_bleed(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(390.0), px(844.0)));
    let fake = connect(&view, cx, 1, "studio");
    let tile = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    cx.run_until_parked();
    let bounds = cx.debug_bounds(selector("item", tile.item)).expect("drawn");
    let strip = cx.debug_bounds("strip").expect("the strip");
    assert_eq!(bounds.size.width, strip.size.width, "edge to edge: {bounds:?} in {strip:?}");
    let window = cx.update(|window, _| window.viewport_size());
    assert!(f32::from(window.height - bounds.bottom()).abs() < 0.5, "to the bottom edge");
    let quads = quads_at(cx, bounds);
    assert!(!quads.is_empty(), "its surface");
    for q in &quads {
        assert_eq!(q.corner_radii.top_left.0, 0.0, "square: {q:?}");
        assert_eq!(q.border_widths.top.0, 0.0, "no ring: {q:?}");
    }
}

/// No body is veiled, focused or not: the header carries the focus, and a quad over every
/// unfocused body would be one more to paint for a wash nobody could see.
#[gpui::test]
fn no_body_is_veiled(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let second = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    let canvas = Theme::default().surfaces.canvas;
    let washes: Vec<gpui::Background> = [alpha::FAINT, alpha::TINT, alpha::PRESSED]
        .into_iter()
        .map(|a| crate::colors::hsla_alpha(canvas, a).into())
        .collect();
    for tile in [first, second] {
        let body = cx.debug_bounds(selector("item", tile.item)).expect("drawn");
        let quads = cx.update(|w, _| w.painted_quads());
        let scale = cx.update(|w, _| w.scale_factor());
        let over = quads.iter().filter(|q| {
            let at = point(px(q.bounds.origin.x.0 / scale), px(q.bounds.origin.y.0 / scale));
            body.contains(&at)
        });
        assert!(over.clone().count() > 0, "the body paints");
        assert!(over.clone().all(|q| !washes.contains(&q.background)), "no wash over a body");
    }
}

/// The header leads with one fixed slot on its inset, where the tile's kind always sits, and
/// its status mark, once there is one, ends it.
#[gpui::test]
fn the_header_leads_with_its_kind_and_ends_with_its_state(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shell = SessionId::new();
    let tile = opens(&view, cx, &fake, shell, fake.me, 1);
    let slot_at = |cx: &mut VisualTestContext| {
        cx.debug_bounds(selector("kind", tile.item)).expect("the slot is always drawn")
    };
    let header = cx.debug_bounds(selector("title", tile.item)).expect("drawn");
    let rest = slot_at(cx);
    let inset = Theme::default().spacing.inset();
    assert!((f32::from(rest.left() - header.left()) - inset).abs() < 0.5, "on the inset");
    assert!(marks(cx).iter().all(|m| m != "Failed"), "at rest: the kind");
    view.update_in(cx, |v, _w, cx| {
        // The failed command's own rows are off screen: only the header can say it.
        let rows = [("$ ", SemanticMark::Prompt { exit: Some(1), input: Some(2) })];
        v.term_event(shell, marked_frame(1, &rows, 0), cx);
    });
    assert!(marks(cx).iter().any(|m| m == "Failed"), "the failure is marked");
    assert_eq!(slot_at(cx), rest, "the kind stays in its square");
    let state = cx.debug_bounds(selector("status", tile.item)).expect("the state's mark");
    let title = cx.debug_bounds(selector("title", tile.item)).expect("the header");
    assert!(state.left() > title.center().x, "at the header's end: {state:?} {title:?}");
}

/// A tile whose agent waits on the human says so once: the waiting glyph that ends its header,
/// a button to the prompt, its words to a screen reader. No bar along the top and no outline
/// repeat it.
#[gpui::test]
fn a_tile_that_needs_you_says_so_once_in_its_header(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let agent = SessionId::new();
    let waiting = opens(&view, cx, &fake, agent, fake.me, 1);
    let other = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
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
                since_ms: WallMs::ZERO,
                mode: None,
            },
            cx,
        );
        v.show_face(agent, false, cx);
    });
    cx.run_until_parked();
    let glyph = cx.debug_bounds(selector("agent", waiting.item)).expect("the glyph");
    let header = cx.debug_bounds(selector("title", waiting.item)).expect("drawn");
    assert!(glyph.top() >= header.top() && glyph.bottom() <= header.bottom(), "in the header");
    // A statement, not a question the click cannot answer; what the click does is said apart.
    let nodes = tree(cx);
    let badge = nodes
        .iter()
        .find(|n| n.label.as_deref() == Some("Needs approval: Wants to run a command"))
        .expect("the glyph says what the agent waits for");
    assert_eq!(badge.role, "Button");
    assert_eq!(badge.description.as_deref(), Some(CHROME_WORDS[0]));
    assert!(cx.debug_bounds(selector("attention", waiting.item)).is_none(), "no bar");
    assert!(cx.debug_bounds(selector("agent", other.item)).is_none(), "only on that one");
    let tile = cx.debug_bounds(selector("item", waiting.item)).expect("drawn");
    let ring = crate::colors::hsla(Theme::default().surfaces.border_subtle);
    let edges = quads_at(cx, tile);
    let mut edges = edges.iter().filter(|q| q.border_widths.left.0 > 0.0);
    assert!(edges.all(|q| q.border_color == ring), "no outline but the panel's ring");
}

/// A header holds no fill at rest: its state is a glyph at its end, not a chip. The leading
/// slot keeps the agent's glyph rather than a warn mark that would say the state again, and an
/// agent the worker guessed at offers nothing to install.
#[gpui::test]
fn a_header_holds_no_fill_and_its_slot_does_not_repeat_its_state(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let agent = SessionId::new();
    let waiting = opens(&view, cx, &fake, agent, fake.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(
            AgentEvent {
                session: agent,
                kind: AgentKind::ClaudeCode,
                status: AgentStatus::Blocked(BlockReason::Question),
                agent_session: None,
                detail: None,
                attention: false,
                source: AgentSource::Transcript,
                since_ms: WallMs::ZERO,
                mode: None,
            },
            cx,
        );
        v.show_face(agent, false, cx);
    });
    cx.run_until_parked();
    let header = cx.debug_bounds(selector("title", waiting.item)).expect("drawn");
    let state = cx.debug_bounds(selector("agent", waiting.item)).expect("the state's glyph");
    let slot = cx.debug_bounds(selector("kind", waiting.item)).expect("the slot");
    let mark = cx.debug_bounds(selector("status", waiting.item)).expect("its mark");
    assert!(state.contains(&mark.center()), "the state is its mark: {mark:?} {state:?}");
    let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
    let inside = |q: &gpui::Quad, b: Bounds<Pixels>| {
        let (x, y) = (q.bounds.origin.x.0 / scale, q.bounds.origin.y.0 / scale);
        let (w, h) = (q.bounds.size.width.0 / scale, q.bounds.size.height.0 / scale);
        x >= f32::from(b.left()) - 0.5
            && y >= f32::from(b.top()) - 0.5
            && x + w <= f32::from(b.right()) + 0.5
            && y + h <= f32::from(b.bottom()) + 0.5
    };
    let surface = |q: &gpui::Quad| {
        let theme = Theme::default();
        [theme.content(), theme.surfaces.panel]
            .iter()
            .any(|c| q.background.as_solid() == Some(crate::colors::hsla(*c)))
    };
    let fills: Vec<&gpui::Quad> = quads
        .iter()
        .filter(|q| inside(q, header) && !q.background.is_transparent() && !surface(q))
        .collect();
    assert!(fills.is_empty(), "no fill in the header: {fills:#?}");
    assert!(cx.debug_bounds(selector("hooks", waiting.item)).is_none(), "no hooks offered");
    let nodes = tree(cx);
    let in_slot = |n: &&crate::a11y::Node| {
        let [x, y, ..] = n.bounds;
        slot.contains(&point(px(x + 1.0), px(y + 1.0)))
    };
    assert!(
        !nodes.iter().filter(in_slot).any(|n| n.is("Image", Some("Needs you"))),
        "the end says it; the slot keeps the agent's glyph"
    );
}

/// One mark says how each tile is doing: its agent's state, a shell's failed last command, and
/// a worker out of reach.
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
                since_ms: WallMs::ZERO,
                mode: None,
            },
            cx,
        );
        // On its TUI, where the header's glyph is a way to its prompt.
        v.show_face(agent, false, cx);
        // The failed command's own rows are off screen: only the header can say it.
        let rows = [("$ ", SemanticMark::Prompt { exit: Some(1), input: Some(2) })];
        v.term_event(shell, marked_frame(1, &rows, 0), cx);
    });
    let drawn = marks(cx);
    // A waiting agent's news ends its header and its navigator row, each once; its slot keeps
    // the agent's glyph.
    assert!(cx.debug_bounds(selector("agent", agent_tile.item)).is_some(), "the agent waits");
    let waiting = drawn.iter().filter(|m| *m == "Needs you").count();
    assert_eq!(waiting, 2, "the glyph in the header, the glyph in the row: {drawn:?}");
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

/// A double-click on the header's empty part fills the screen with its tile, as ⌃⌘F does and
/// a Mac's title bar zooms its window; one on its name names it. Close, under the pointer, does
/// what ⌘W does.
#[gpui::test]
fn the_header_fills_the_screen_names_and_closes_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let _first = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let second = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    let width = |cx: &mut VisualTestContext| {
        f32::from(cx.debug_bounds(selector("item", second.item)).expect("drawn").size.width)
    };
    let before = width(cx);
    let name = cx.debug_bounds(selector("name", second.item)).expect("its name");
    let empty = point(name.right() + px(24.0), name.center().y);
    double_click(cx, empty);
    assert!(width(cx) > before + 100.0, "fullscreen: {before} → {}", width(cx));
    let name = cx.debug_bounds(selector("name", second.item)).expect("drawn");
    double_click(cx, name.center());
    let renaming = view.read_with(cx, |v, _| v.rename.as_ref().map(|r| r.tile));
    assert_eq!(renaming, Some(second), "a double-click on the name names the tile");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let header = cx.debug_bounds(selector("title", second.item)).expect("drawn");
    cx.simulate_mouse_move(header.center(), None, Modifiers::none());
    cx.run_until_parked();

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

/// The one word for a worker being dialled again, then for how long once that is worth
/// saying, in ten-second steps, whole minutes past an hour.
#[test]
fn reconnecting_says_for_how_long() {
    let at = |secs| reconnecting(Duration::from_secs(secs));
    assert_eq!(at(0), RECONNECTING);
    assert_eq!(at(9), RECONNECTING);
    assert_eq!(at(10), "Reconnecting for 10 s");
    assert_eq!(at(59), "Reconnecting for 50 s");
    assert_eq!(at(75), "Reconnecting for 1m 10s");
    assert_eq!(at(3_725), "Reconnecting for 1h 2m");
}

/// A tile's pill counts the time its worker has been out of reach on the workspace's clock,
/// from when the link dropped; the navigator says the same one word, from the first dial on.
#[gpui::test]
fn a_tile_says_how_long_its_worker_has_been_away(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let _tile = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let key = fake.key;
    let start = Duration::from_secs(100);
    view.update_in(cx, |v, _w, cx| {
        v.hold_clock(Some(start));
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
    let said = |cx: &mut VisualTestContext, words: &str| {
        tree(cx).iter().any(|n| n.is("Status", Some(words)))
    };
    assert!(said(cx, RECONNECTING), "at once, the one word");
    view.update(cx, |v, cx| {
        v.hold_clock(Some(start.saturating_add(Duration::from_secs(75))));
        cx.notify();
    });
    cx.run_until_parked();
    assert!(said(cx, "Reconnecting for 1m 10s"), "{:#?}", tree(cx));
    let word = |status| navigator::worker_health(&status).map(|(_, w)| w);
    assert_eq!(word(WorkerStatus::Connecting), Some("reconnecting"), "a first dial too");
    assert_eq!(word(WorkerStatus::Reconnecting("lost".into())), Some("reconnecting"));
}

/// A tile whose worker runs another build says so calmly at its foot: the title, both builds,
/// and the command that updates it, which one press copies. The worker's line says it too.
#[gpui::test]
fn a_worker_on_another_build_offers_the_command_that_updates_it(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let peer = "0.0.9+wire.0badf00d".to_owned();
    let notice = UpdateNotice { of: Of::Worker, host: "studio".to_owned(), peer };
    let key = fake.key;
    let status = WorkerStatus::NeedsUpdate(notice.clone());
    assert_eq!(status.text(), "runs a different build");
    view.update_in(cx, |v, _w, cx| v.disconnect_worker(key, status, cx));
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Status", Some(notice.title()))), "{nodes:#?}");
    let drawn = cx.debug_bounds(selector("state", tile.item)).expect("the pill");
    assert!(drawn.size.width > px(200.0), "the builds are said beside the title: {drawn:?}");
    assert!(nodes.iter().any(|n| n.is("Button", Some(COPY_COMMAND))), "{nodes:#?}");
    click(cx, selector("copy-command", tile.item));
    let copied = cx.update(|_w, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(copied.as_deref(), Some("slopty worker deploy studio --update"));
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
        name: None,
        facts: BTreeMap::new(),
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
        v.apply_sync(key, ItemSync::Delta { version: 1, by: me, op: ItemOp::Add(item) }, cx);
    });
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Status", Some("Exited · code 2"))), "{nodes:#?}");
    assert!(cx.debug_bounds(selector("close-ended", tile.item)).is_some(), "Close is offered");
    fake.drain();

    click(cx, selector("restart", tile.item));
    let sent = fake.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::OpenSession { spec: o, .. }
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

/// Notices stack in the title bar's lane on its right, no wider than 400 pt, two at most (a
/// third pushes the oldest out), and each goes after six seconds.
#[gpui::test]
fn notices_stack_two_in_the_title_bar_and_go(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _fake = connect(&view, cx, 1, "studio");
    let long = "a word that goes on ".repeat(10);
    for text in ["one", "two", long.as_str()] {
        view.update_in(cx, |v, _w, cx| v.show_notice(text.to_owned(), cx));
        cx.executor().advance_clock(Duration::from_secs(1));
    }
    cx.run_until_parked();
    let shown = view.read_with(cx, |v, _| v.toast_texts());
    assert_eq!(shown.len(), SHOWN);
    assert_eq!(shown, vec!["two".to_owned(), long.clone()], "the oldest went");

    let nodes = tree(cx);
    let [x, y, w, _] = nodes
        .iter()
        .find(|n| n.is("Status", Some(long.as_str())))
        .map_or_else(|| panic!("the long notice is drawn: {nodes:#?}"), |n| n.bounds);
    let strip = cx.debug_bounds("workspace").expect("drawn");
    let titlebar = cx.debug_bounds("titlebar").expect("drawn");
    assert!(w <= 400.0, "capped at 400 pt: {w}");
    assert!(x > f32::from(strip.center().x), "on the right: {x}");
    assert!(y < f32::from(titlebar.bottom()), "in the title bar: {y}");

    cx.executor().advance_clock(SAY_FOR);
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.toast_texts()).is_empty(), "gone after 6 s");
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
        sent.iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Add(i)) if i.id == third.item)),
        "{sent:?}"
    );
    assert!(view.read_with(cx, |v, _| v.layout().contains(third)), "back");
    assert!(cx.debug_bounds("closed").is_none(), "the offer is taken");
}

/// A header at rest has no buttons, focused or not: its state's glyph ends it. Under the
/// pointer the face toggle and close stand over the glyph's place on the header's ground,
/// keeping no room at rest; and the swap moves nothing.
#[gpui::test]
fn a_header_at_rest_has_no_buttons(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let agent = SessionId::new();
    let waiting = opens(&view, cx, &fake, agent, fake.me, 1);
    let other = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    // The face toggle is one of the controls; its tile shows the TUI.
    agent_thread(&view, cx, fake.key, agent);
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
                since_ms: WallMs::ZERO,
                mode: None,
            },
            cx,
        );
    });
    view.update_in(cx, |v, _w, cx| v.show_face(agent, false, cx));
    cx.simulate_mouse_move(point(px(1.0), px(799.0)), None, Modifiers::none());
    cx.run_until_parked();
    let bounds = |cx: &mut VisualTestContext, part: &str| {
        cx.debug_bounds(selector(part, waiting.item)).unwrap_or_else(|| panic!("{part} drawn"))
    };
    let (header, name, glyph) = (bounds(cx, "title"), bounds(cx, "name"), bounds(cx, "agent"));
    let inset = Theme::default().spacing.inset();
    assert!(
        (f32::from(header.right() - glyph.right()) - inset).abs() < 0.5,
        "the state ends the header at rest: {glyph:?} in {header:?}"
    );
    assert!(name.right() <= glyph.left(), "the title ends before it: {name:?} {glyph:?}");
    // Neither the focused tile nor the other draws a control: close and the face toggle are
    // there for the keyboard and a screen reader, and drawn only under the pointer.
    for tile in [waiting, other] {
        for part in ["close", "face-thread", "face-terminal"] {
            let drawn = cx.debug_bounds(selector(part, tile.item));
            assert!(drawn.is_none(), "no {part} at rest: {drawn:?}");
        }
    }

    cx.simulate_mouse_move(header.center(), None, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(bounds(cx, "name"), name, "hovered: the title stays");
    let controls = bounds(cx, "controls");
    assert!(controls.right() <= glyph.right() + px(0.5), "over the glyph's place: {controls:?}");
    let close = cx.debug_bounds(selector("close", waiting.item)).expect("close drawn");
    assert!(controls.contains(&close.center()), "close is one of them");
    let face = cx.debug_bounds(selector("face-thread", waiting.item)).expect("the face toggle");
    assert!(controls.contains(&face.center()), "the toggle is one of the controls");
    assert!(face.right() <= close.left(), "before close: {face:?} {close:?}");
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Button", Some("Show thread"))), "named for its face");
}

/// A tabbed column's header is a tab row: a tab per tile with its title, the shown one
/// selected, each tab's kind on the edge grid a single header's is on; a click on a tab shows
/// and focuses it, and a tab's close closes its tile.
#[gpui::test]
fn a_tabbed_column_draws_a_tab_per_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let second = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    let single = f32::from(
        cx.debug_bounds(selector("kind", first.item)).expect("the slot").left()
            - cx.debug_bounds(selector("title", first.item)).expect("the header").left(),
    );
    let inset = Theme::default().spacing.inset();
    assert!((single - inset).abs() < 0.5, "a single header's slot on the grid: {single}");
    cx.simulate_keystrokes("cmd-[");
    cx.simulate_keystrokes("cmd-alt-t");
    cx.run_until_parked();
    assert_eq!(column_of(&view, cx, first), column_of(&view, cx, second), "one column");
    assert_eq!(focused(&view, cx), Some(second));
    let tab = |cx: &mut VisualTestContext, tile: TileRef| {
        cx.debug_bounds(selector("tab", tile.item)).expect("a tab per tile")
    };
    let (a, b) = (tab(cx, first), tab(cx, second));
    assert_eq!(a.top(), b.top(), "side by side in one row");
    let slot = cx.debug_bounds(selector("tab-slot", first.item)).expect("the tab's slot");
    let row = cx.debug_bounds(selector("title", second.item)).expect("the column's header");
    let tabbed = f32::from(slot.left() - row.left());
    assert!((tabbed - single).abs() < 0.5, "a tab's slot on the same grid: {tabbed}");
    assert!(a.right() <= b.left(), "in the column's order");
    let tabs: Vec<_> = tree(cx).into_iter().filter(|n| n.role == "Tab").collect();
    assert_eq!(tabs.len(), 2, "{tabs:#?}");
    assert!(
        tree(cx).iter().all(|n| n.label.as_deref() != Some("2/2")),
        "no count stands in for the tabs"
    );

    cx.simulate_click(a.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(first), "the clicked tab is shown and focused");
    let close = cx.debug_bounds(selector("tab-close", second.item)).expect("drawn");
    cx.simulate_mouse_move(close.center(), None, Modifiers::none());
    cx.simulate_click(close.center(), Modifiers::none());
    cx.run_until_parked();
    let left: Vec<TileRef> = view.read_with(cx, |v, _| v.layout().tiles().collect());
    assert_eq!(left, vec![first], "its close closed that tab's tile");
}

/// Four shells in one tabbed column, the last shown, beside a column of their own so the tabs
/// are the column's header, in a window wide enough for both at any width. The tabs, in order.
fn four_tabs(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<TileRef> {
    cx.simulate_resize(size(px(1600.0), px(800.0)));
    let fake = connect(view, cx, 1, "studio");
    let tabs: Vec<TileRef> =
        (1..=4).map(|version| opens(view, cx, &fake, SessionId::new(), fake.me, version)).collect();
    view.update_in(cx, |v, _w, cx| {
        for tab in tabs.iter().skip(1) {
            v.focus_tile(*tab, cx);
            v.layout.consume_or_expel_window_left();
        }
        v.layout.toggle_tabbed();
        cx.notify();
    });
    cx.run_until_parked();
    opens(view, cx, &fake, SessionId::new(), fake.me, 99);
    let last = *tabs.last().expect("four tabs");
    view.update_in(cx, |v, _w, cx| v.focus_tile(last, cx));
    cx.run_until_parked();
    tabs
}

/// `tile`'s column dragged to `width`, as its divider is, and drawn twice so a scroll asked for
/// lands.
fn column_at(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tile: TileRef, width: f32) {
    for _ in 0..4 {
        let got =
            view.read_with(cx, |v, _| v.tile_bounds(tile)).map_or(0.0, |b| f32::from(b.size.width));
        if (got - width).abs() < 0.5 {
            break;
        }
        let column = column_of(view, cx, tile);
        view.update_in(cx, |v, _w, cx| {
            v.layout.resize_begin(column);
            v.layout.resize_update(width - got);
            v.layout.resize_end();
            v.focus_tile(tile, cx);
            cx.notify();
        });
        cx.run_until_parked();
    }
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// Whether `tab`'s tab lies wholly inside `column`'s tile.
fn tab_in_view(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tab: TileRef) -> bool {
    let tile = view.read_with(cx, |v, _| v.tile_bounds(tab)).expect("the column is drawn");
    let at = cx.debug_bounds(selector("tab", tab.item)).expect("the tab is drawn");
    at.left() >= tile.left() - px(0.5) && at.right() <= tile.right() + px(0.5)
}

/// A narrow tabbed column scrolls its shown tab into view: the last of four at 312 pt, after
/// the column narrowed from a width where every tab fitted, and again after another tab was
/// shown and the last shown back. The strip never leaves the tab in hand past its edge.
#[gpui::test]
fn a_narrow_tab_row_keeps_its_shown_tab_in_view(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let tabs = four_tabs(&view, cx);
    let (first, last) = (tabs[0], tabs[3]);
    column_at(&view, cx, last, 720.0);
    assert!(tab_in_view(&view, cx, last), "every tab fits at 720 pt");
    column_at(&view, cx, last, 312.0);
    assert!(tab_in_view(&view, cx, last), "the shown tab stays in view as the column narrows");
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(tab_in_view(&view, cx, first), "the first, shown, is in view");
    view.update_in(cx, |v, _w, cx| v.focus_tile(last, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(tab_in_view(&view, cx, last), "and the last again, once shown");
}

/// A tab not shown keeps its close out of its row at rest, so a narrow tab keeps its mark and
/// its four letters' room: its name runs to the tab's end. Its close is still drawn, over the
/// tab's end, for the pointer and the keyboard. The shown tab keeps its close in its row and
/// its letters' room beside it.
#[gpui::test]
fn a_tab_not_shown_keeps_its_close_out_of_its_room(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let tabs = four_tabs(&view, cx);
    let last = tabs[3];
    column_at(&view, cx, last, 312.0);
    let theme = Theme::default();
    for tab in tabs.iter().take(3) {
        let at = cx.debug_bounds(selector("tab", tab.item)).expect("the tab is drawn");
        let name = cx.debug_bounds(selector("name", tab.item)).expect("its name is drawn");
        let pad = theme.spacing.xxs + 0.5;
        assert!(name.right() >= at.right() - px(pad), "the name runs to the end: {name:?} {at:?}");
        let letters = f32::from(name.size.width);
        assert!(letters >= theme.typography.ui_size * 2.0, "room for its letters: {letters}");
        // Under the pointer its close shows, over the tab's end.
        cx.simulate_mouse_move(at.center(), None, Modifiers::none());
        cx.run_until_parked();
        let close = cx.debug_bounds(selector("tab-close", tab.item)).expect("its close is drawn");
        assert!(close.left() >= at.left() && close.right() <= at.right(), "over the tab's end");
    }
    let shown = cx.debug_bounds(selector("name", last.item)).expect("drawn");
    let close = cx.debug_bounds(selector("tab-close", last.item)).expect("drawn");
    assert!(shown.right() <= close.left(), "the shown tab keeps its close in its row");
    let letters = f32::from(shown.size.width);
    assert!(letters >= theme.typography.ui_size * 2.0, "and its letters' room: {letters}");
}

/// On a phone a column is the screen's width already and the tile has no header: its rows are
/// the bar's "…", which offers to close it but not to fill the screen it fills. On a desktop,
/// where a column is part of the strip, the header's menu offers fullscreen.
#[gpui::test]
fn a_tile_that_fills_a_phone_offers_no_fullscreen(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let header = cx.debug_bounds(selector("title", shell.item)).expect("drawn");
    cx.simulate_mouse_down(header.center(), MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(header.center(), MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    let rows: Vec<String> =
        tree(cx).into_iter().filter(|n| n.role == "MenuItem").filter_map(|n| n.label).collect();
    assert!(rows.iter().any(|r| r == "Fullscreen"), "on a desktop: {rows:?}");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    click(cx, "more");
    let rows: Vec<String> =
        tree(cx).into_iter().filter(|n| n.role == "MenuItem").filter_map(|n| n.label).collect();
    assert!(rows.iter().any(|r| r == "Close tile"), "close stays: {rows:?}");
    assert!(!rows.iter().any(|r| r == "Fullscreen"), "a phone's column already does: {rows:?}");
}

/// Two shells of one worker that would read alike are told apart, in the order they were
/// made: the second is "Terminal 2" in its header and wherever the tile is named. Another
/// worker's shell starts its own count, and a tile given a name keeps it.
#[gpui::test]
fn tiles_that_read_alike_are_numbered(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    let laptop = connect(&view, cx, 2, "laptop");
    let other = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 1);
    cx.run_until_parked();
    let titles =
        view.read_with(cx, |v, _| [first, second, other].map(|t| v.tile_title(v.item(t).unwrap())));
    assert_eq!(titles, ["Terminal", "Terminal 2", "Terminal"]);
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Heading", Some("terminal Terminal 2"))), "{nodes:#?}");
}

/// A shell titled by the directory it stands in shows no place: the directory above it read as
/// the cwd, and the breadcrumb has the whole path.
#[test]
fn a_place_does_not_repeat_the_title() {
    use crate::workspace::tile::place_beside;
    let beside = |place: &str, title: &str| place_beside(place.to_owned(), title);
    assert_eq!(beside("~/src/slopty", "slopty"), None);
    assert_eq!(beside("~/drop-here", "drop-here"), None);
    assert_eq!(beside("/etc", "etc"), None);
    assert_eq!(beside("slopty", "slopty"), None);
    assert_eq!(beside("~", "Terminal").as_deref(), Some("~"));
    assert_eq!(beside("~/src/myslopty", "slopty").as_deref(), Some("~/src/myslopty"));
}

/// A tile's leading slot at rest shows its kind: an idle agent keeps its glyph rather than a
/// hollow ring that read as an unticked radio button.
#[gpui::test]
fn an_idle_agent_keeps_its_kind_in_the_slot(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let agent = SessionId::new();
    let tile = opens(&view, cx, &fake, agent, fake.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Idle, ..blocked(agent) }, cx);
    });
    cx.run_until_parked();
    let slot = cx.debug_bounds(selector("kind", tile.item)).expect("the slot");
    assert!(cx.debug_bounds(selector("status", tile.item)).is_none(), "no state at rest");
    let idle = crate::icons::Status::Idle.label();
    let marked = tree(cx).into_iter().any(|n| {
        let [x, y, ..] = n.bounds;
        slot.contains(&point(px(x + 1.0), px(y + 1.0))) && n.is("Image", Some(idle))
    });
    assert!(!marked, "the slot keeps the agent's glyph");
}

/// A long command that ended well while the human looked elsewhere reads in its header as
/// the time it took alone, in the meta size: the slot's check says it is done, and that it went
/// unseen is the navigator's to say. A screen reader still hears all of it.
#[gpui::test]
fn a_finished_command_reads_as_its_time_alone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, fake.me, 1);
    let _other = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    let done = Finished { command: "make".into(), exit: Some(0), elapsed: Duration::from_secs(6) };
    let full = done.label();
    view.update_in(cx, |v, _w, cx| {
        v.finished.insert(session, done);
        cx.notify();
    });
    cx.run_until_parked();
    let readout = cx.debug_bounds(selector("finished", tile.item)).expect("the readout");
    assert!(tree(cx).iter().any(|n| n.is("Button", Some(full.as_str()))), "all of it, said");
    let theme = Theme::default();
    let words = |cx: &mut VisualTestContext, text: &str| {
        cx.update(|window, _| {
            let run = window.text_style().to_run(text.len());
            let size = px(theme.typography.small());
            window.text_system().shape_line(text.to_owned().into(), size, &[run], None).width
        })
    };
    let took = words(cx, &crate::kit::duration(Duration::from_secs(6)));
    let pad = px(2.0 * theme.spacing.xs);
    assert!(readout.size.width <= took + pad + px(0.5), "the time alone: {readout:?}");
    assert!(cx.debug_bounds(selector("unseen", tile.item)).is_none(), "no dot on screen");
}

/// In the overview the words line up with the panes: a workspace's name and the "New
/// workspace" glyph start on the edge of the glyphs the pane covers lead with.
#[gpui::test]
fn the_overview_words_start_on_the_panes_glyphs(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    cx.simulate_keystrokes("cmd-alt-o");
    cx.run_until_parked();
    let edge = shells
        .iter()
        .filter_map(|(_, t)| cx.debug_bounds(selector("shapes-label", t.item)))
        .map(|b| f32::from(b.left()))
        .fold(f32::INFINITY, f32::min);
    let name = cx.debug_bounds("overview-name-0").expect("the name");
    assert!((f32::from(name.left()) - edge).abs() < 0.5, "{name:?} on {edge}");
    let new = cx.debug_bounds("overview-new-workspace").expect("the place for the next");
    let pad = Theme::default().spacing.sm;
    assert!((f32::from(new.left()) + pad - edge).abs() < 0.5, "its glyph on {edge}: {new:?}");
}

/// On a phone the bar is the focused tile's, as a navigation bar names its screen: its kind and
/// its title, as an inline navigation title, a step above the rows by weight as the drawer's is,
/// and the tile draws no header of its own, so the screen keeps one bar.
/// There is no "+": the tile's own rows lead the "…" menu, then what "+" opened.
#[gpui::test]
fn a_phone_bar_is_a_navigation_bar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    assert!(cx.debug_bounds(selector("title", shell.item)).is_some(), "a header on a desktop");
    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("title", shell.item)).is_none(), "no header on a phone");
    assert!(cx.debug_bounds(selector("phone-kind", shell.item)).is_some(), "the tile's kind");
    let title = view.read_with(cx, |v, _| v.item(shell).map(|i| v.tile_title(i))).unwrap();
    let heading = tree(cx).into_iter().find(|n| {
        n.role == "Heading" && n.label.as_deref() == Some(format!("terminal {title}").as_str())
    });
    assert!(heading.is_some(), "the bar is titled as the tile's header is: {title}");
    assert!(cx.debug_bounds("new-menu").is_none(), "no +");
    let name = cx.debug_bounds("phone-title").expect("the focused tile's title");
    assert!(cx.debug_bounds("breadcrumb").is_none(), "the title alone, no breadcrumb");
    let role = view.read_with(cx, |v, _| titlebar::phone_title_role(&v.theme));
    assert!(
        (name.size.height - px(role.line)).abs() < px(0.5),
        "an inline navigation title: {name:?}"
    );
    click(cx, "more");
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let rows: Vec<String> =
        tree(cx).into_iter().filter(|n| n.role == "MenuItem").filter_map(|n| n.label).collect();
    assert_eq!(rows.first().map(String::as_str), Some("Rename"), "the tile's rows: {rows:?}");
    assert!(rows.iter().any(|r| r == "New terminal"), "{rows:?}");
    assert!(rows.iter().any(|r| r == "New workspace"), "{rows:?}");
}

/// A phone names its tile in the bar, which is the tile's header there: "Name this tile" turns
/// the bar's title into the field, named as a header's is, and ↩ keeps the name it was given.
#[gpui::test]
fn a_phone_names_its_tile_in_the_bar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    view.update_in(cx, |v, window, cx| v.rename_item(&RenameItem, window, cx));
    cx.run_until_parked();
    let field = tree(cx)
        .into_iter()
        .find(|n| n.role == "TextInput" && n.label.as_deref() == Some("Tile name"));
    assert!(field.is_some(), "the bar holds the field");
    assert!(cx.debug_bounds(selector("rename", shell.item)).is_some());
    assert!(cx.debug_bounds(selector("phone-kind", shell.item)).is_some(), "beside its kind");
    cx.simulate_input("scratch");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let named = tree(cx)
        .into_iter()
        .any(|n| n.role == "Heading" && n.label.as_deref() == Some("terminal scratch"));
    assert!(named, "named, the bar says so: {:?}", tree(cx));
    assert!(cx.debug_bounds(selector("rename", shell.item)).is_none(), "and the field is gone");
}

/// Chrome moves where it may: a menu drops in, a notice rises in and fades when its time is
/// up, and a closing tile fades where it stood. Under Reduce Motion each lands at once.
#[gpui::test]
fn chrome_moves_and_holds_still_under_reduce_motion(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &fake);
    view.update(cx, |v, _| v.set_animation(true));
    let menu_top = |cx: &mut VisualTestContext| {
        click(cx, "more");
        let top = cx.debug_bounds("menu").expect("the menu").top();
        click(cx, "more");
        top
    };
    let notice_top = |cx: &mut VisualTestContext, text: &str| {
        view.update_in(cx, |v, _w, cx| v.show_notice(text.to_owned(), cx));
        cx.run_until_parked();
        cx.debug_bounds("said").expect("the notice").top()
    };

    let (dropping, rising) = (menu_top(cx), notice_top(cx, "one"));
    cx.executor().advance_clock(SAY_FOR);
    cx.run_until_parked();
    assert!(cx.debug_bounds("said").is_some(), "time's up: it fades where it stands");
    assert!(view.read_with(cx, |v, _| v.toast_text()).is_none(), "and is no longer up");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert!(cx.debug_bounds("said").is_none(), "then it goes");
    let last = focused(&view, cx).expect("a focused shell");
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    let fading = Box::leak(format!("closing-{}", last.item.as_uuid()).into_boxed_str());
    assert!(cx.debug_bounds(fading).is_some(), "the closed tile fades where it stood");
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    let (still_menu, still_notice) = (menu_top(cx), notice_top(cx, "two"));
    assert!(dropping < still_menu - px(1.0), "the menu dropped in: {dropping:?} {still_menu:?}");
    assert!(rising > still_notice + px(1.0), "the notice rose in: {rising:?} {still_notice:?}");
    cx.executor().advance_clock(SAY_FOR);
    cx.run_until_parked();
    assert!(cx.debug_bounds("said").is_none(), "gone at once");
    let next = focused(&view, cx).expect("a shell left to close");
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    let gone = Box::leak(format!("closing-{}", next.item.as_uuid()).into_boxed_str());
    assert!(cx.debug_bounds(gone).is_none(), "the tile goes at once");
}

/// The overview's words show only while it opens (fading in once the zoom has all but landed)
/// and leave at once as it closes; where chrome does not move they are simply there.
#[test]
fn the_overview_words_wait_for_the_zoom() {
    use crate::workspace::strip::overview_words;
    assert!(overview_words(gpui::div(), "w", true, true).is_some(), "opening: fading in");
    assert!(overview_words(gpui::div(), "w", true, false).is_some(), "still: at once");
    assert!(overview_words(gpui::div(), "w", false, true).is_none(), "closing: gone");
}

/// The server's word says what it costs, to a screen reader and under the pointer, and is a
/// button: pressed, it offers what can be done.
#[gpui::test]
fn the_servers_word_says_what_it_costs(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update_in(cx, |v, _w, cx| v.set_server_status(Some("server offline".into()), cx));
    cx.run_until_parked();
    let server = tree(cx).into_iter().find(|n| n.is("Button", Some("Server offline")));
    let server = server.expect("the server's word");
    assert_eq!(server.description.as_deref(), Some("Machines you reach directly still work"));
    assert!(cx.debug_bounds("readout-server").is_some(), "drawn");
}

/// Inside a repository a place is named by it and the path within; elsewhere, the tail.
#[test]
fn a_place_in_a_repository_is_named_by_it() {
    use crate::workspace::tile::repo_place;
    assert_eq!(repo_place("/w/oss/slopty", Some("/w/oss/slopty"), None), "slopty");
    assert_eq!(
        repo_place("/w/oss/slopty/crates/ui/", Some("/w/oss/slopty"), None),
        "slopty/crates/ui"
    );
    assert_eq!(repo_place("/w/oss/slopty-two", Some("/w/oss/slopty"), None), "oss/slopty-two");
    assert_eq!(repo_place("/Users/me/src", None, None), "~/src");
}

/// The empty workspace's quieter ways to begin say where they open.
#[gpui::test]
fn the_ways_to_begin_say_where_they_open(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _studio = connect(&view, cx, 1, "studio");
    cx.run_until_parked();
    for row in ["empty-terminal", "empty-window"] {
        let at = cx.debug_bounds(row).unwrap_or_else(|| panic!("{row}"));
        let target = cx
            .debug_bounds(Box::leak(format!("{row}-target").into_boxed_str()))
            .unwrap_or_else(|| panic!("{row} names its worker"));
        assert!(at.contains(&target.center()), "{row}: {at:?} {target:?}");
    }
}

/// A remote window on its way turns its mark in its body, over what opens and where, and not
/// in its header's slot as well.
#[gpui::test]
fn an_opening_window_turns_its_mark_in_the_body(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window: slopty_core::WindowId(7) }, 1);
    cx.executor().advance_clock(crate::screen::LOADING_GRACE);
    cx.run_until_parked();
    let working = crate::icons::Status::Working.label();
    let slot = cx.debug_bounds(selector("kind", tile.item)).expect("the slot");
    assert!(cx.debug_bounds(selector("status", tile.item)).is_none(), "no header state");
    let body = cx.debug_bounds(selector("waiting", tile.item)).expect("the body's block");
    let nodes = tree(cx);
    let in_slot = |n: &crate::a11y::Node| {
        let [x, y, ..] = n.bounds;
        slot.contains(&point(px(x + 1.0), px(y + 1.0)))
    };
    assert!(!nodes.iter().any(|n| in_slot(n) && n.is("Image", Some(working))), "no header mark");
    assert!(body.size.height > px(40.0), "a mark and two lines: {body:?}");
    assert!(nodes.iter().any(|n| n.is("Status", Some("Opening Window 7 on studio…"))));
}

/// Shells of one worker that would read alike are told apart by the command each last ran
/// before a number is needed: "make", "cargo test" (its `cd` dropped) and the one that ran
/// nothing, which needs no number once it is alone. The second line does not say the command
/// again.
#[gpui::test]
fn shells_that_read_alike_are_named_by_their_last_command(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let sessions = [SessionId::new(), SessionId::new(), SessionId::new()];
    let tiles = [
        opens(&view, cx, &studio, sessions[0], studio.me, 1),
        opens(&view, cx, &studio, sessions[1], studio.me, 2),
        opens(&view, cx, &studio, sessions[2], studio.me, 3),
    ];
    for (session, command) in [(sessions[0], "make"), (sessions[1], "cd ~/srv && cargo test")] {
        let done =
            Finished { command: command.into(), exit: Some(0), elapsed: Duration::from_secs(40) };
        view.update_in(cx, |v, _w, cx| v.command_finished(session, done, cx));
    }
    cx.run_until_parked();
    let titles = view.read_with(cx, |v, _| tiles.map(|t| v.tile_title(v.item(t).unwrap())));
    assert_eq!(titles, ["make", "cargo test", "Terminal"]);
    let lines = view.read_with(cx, WorkspaceView::navigator_lines);
    let make = lines.iter().find(|(title, ..)| title == "make").expect("its row");
    assert!(!make.1.contains("make"), "said once: {make:?}");
}

/// A tile that wears an agent's mark is announced by its agent, as it shows: its header's
/// heading, and on a phone the bar's, lead with the agent's name, where a shell's lead with
/// "terminal". Twins are still numbered by their kind.
#[gpui::test]
fn an_agents_tile_is_announced_by_its_agent(cx: &mut TestAppContext) {
    use slopty_proto::thread::Cursor;
    use slopty_proto::thread::wire::TableFrame;

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let agent = opens(&view, cx, &studio, session, studio.me, 1);
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table, cx));
    cx.run_until_parked();
    let title = |tile: TileRef, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.item(tile).map(|i| v.tile_title(i))).expect("its title")
    };
    let (agent_title, shell_title) = (title(agent, cx), title(shell, cx));
    let label = crate::conversation::thread::view::agent_label(&state.meta.agent);
    let headings: Vec<String> =
        tree(cx).into_iter().filter(|n| n.role == "Heading").filter_map(|n| n.label).collect();
    let spoken = format!("{label} {agent_title}");
    assert!(headings.iter().any(|h| h.starts_with(&spoken)), "{spoken:?} in {headings:#?}");
    let plain = format!("terminal {shell_title}");
    assert!(headings.iter().any(|h| h.starts_with(&plain)), "{plain:?} in {headings:#?}");

    view.update_in(cx, |v, _w, cx| v.focus_tile(agent, cx));
    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    let bar = tree(cx).into_iter().find(|n| {
        n.role == "Heading" && n.label.as_deref().is_some_and(|l| l.starts_with(&spoken))
    });
    assert!(bar.is_some(), "the phone's bar says it the same way");
}

/// A heading leads with the tile's kind or agent unless its title already says it: a twin
/// numbered "Claude Code 2" is not "Claude Code Claude Code 2".
#[gpui::test]
fn a_heading_never_says_its_agent_twice(_cx: &mut TestAppContext) {
    use crate::workspace::tile::spoken_heading;
    assert_eq!(spoken_heading("Claude Code", "Claude Code 2"), "Claude Code 2");
    assert_eq!(spoken_heading("pi", "pi"), "pi");
    assert_eq!(spoken_heading("Claude Code", "Fix the login"), "Claude Code Fix the login");
    assert_eq!(spoken_heading("pi", "pilot run"), "pi pilot run", "a word, not a prefix");
}

/// A double-click of the left button at `at`.
fn double_click(cx: &mut VisualTestContext, at: Point<Pixels>) {
    let (left, modifiers) = (MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(at, None, modifiers);
    for click_count in [1, 2] {
        let down = gpui::MouseDownEvent {
            button: left,
            position: at,
            modifiers,
            click_count,
            first_mouse: false,
        };
        cx.simulate_event(down);
        cx.simulate_event(gpui::MouseUpEvent {
            button: left,
            position: at,
            modifiers,
            click_count,
        });
    }
    cx.run_until_parked();
}

//! The palette's sections, the picker's waiting row, the empty workspace and the overview's
//! labels, in the headless workspace.

use gpui::Modifiers;
use slopty_core::WindowId;
use slopty_proto::screen::{DisplayInfo, WindowInfo};

use super::*;

/// The top edge of `selector` in the last frame, if it was drawn.
fn top(cx: &mut VisualTestContext, selector: &'static str) -> Option<f32> {
    cx.debug_bounds(selector).map(|b| f32::from(b.origin.y))
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at =
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn")).center();
    cx.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
}

fn open_palette(cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_some(), "the palette is up");
}

/// The palette lists the tiles first, then the workers, then the commands, each group under
/// its heading, with the worker named on a tile's line once there are two of them, and a
/// waiting agent marked. A query that leaves a group empty hides its heading, and one that
/// leaves a single group needs none.
#[gpui::test]
fn the_palette_lists_tiles_then_workers_then_commands(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _laptop = connect(&view, cx, 2, "laptop");
    let session = SessionId::new();
    opens(&view, cx, &studio, session, studio.me, 1);
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    cx.run_until_parked();
    cx.update(|window, _cx| window.set_a11y_active(true));
    open_palette(cx);

    let (tiles, workers, commands) = (
        top(cx, "palette-heading-tiles").expect("a tiles heading"),
        top(cx, "palette-heading-workers").expect("a workers heading"),
        top(cx, "palette-heading-commands").expect("a commands heading"),
    );
    assert!(tiles < workers && workers < commands, "{tiles} {workers} {commands}");
    let lines = view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("open");
        palette
            .read(cx)
            .matches(cx)
            .into_iter()
            .map(|l| (l.section, l.a11y_label(), l.status))
            .collect::<Vec<_>>()
    });
    let first = lines.first().expect("lines");
    assert_eq!(first.0, crate::palette::Section::Tiles, "{lines:#?}");
    assert!(first.1.contains("studio"), "the worker is named: {lines:#?}");
    assert_eq!(first.2, Some(crate::icons::Status::NeedsYou), "the agent is marked");
    let mut grouped: Vec<_> = lines.iter().map(|l| l.0).collect();
    grouped.dedup();
    assert_eq!(
        grouped,
        [
            crate::palette::Section::Tiles,
            crate::palette::Section::Workers,
            crate::palette::Section::Commands
        ],
        "each group once, in order"
    );
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Heading", Some("Workers"))), "{tree:#?}");
    assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("New terminal ⌘T"))), "{tree:#?}");

    // "go to" is in the tiles' and the workers' lines, and in no command's.
    cx.simulate_keystrokes("g o space t o");
    cx.run_until_parked();
    assert!(top(cx, "palette-heading-tiles").is_some());
    assert!(top(cx, "palette-heading-workers").is_some());
    assert!(top(cx, "palette-heading-commands").is_none(), "an empty group hides its heading");

    // Only the workers are left: one group, no heading at all.
    cx.simulate_keystrokes("space l a p");
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette-item-0").is_some(), "the laptop's line");
    assert!(cx.debug_bounds("palette-item-1").is_none());
    for heading in ["palette-heading-tiles", "palette-heading-workers", "palette-heading-commands"]
    {
        assert!(top(cx, heading).is_none(), "{heading} over a lone group");
    }
    // ↩ runs what is shown: the laptop's line, the one left.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_none());
}

/// Every line's title starts on one edge: the kind icon sits in a fixed slot, and a status
/// mark or a worker's name goes on the right, not before the title.
#[gpui::test]
fn the_icon_slot_keeps_every_title_on_one_edge(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _laptop = connect(&view, cx, 2, "laptop");
    let waiting = SessionId::new();
    opens(&view, cx, &studio, waiting, studio.me, 1);
    opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(waiting), cx));
    cx.run_until_parked();
    open_palette(cx);
    let marked = view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("open");
        palette.read(cx).matches(cx).iter().take(6).map(|l| l.status.is_some()).collect::<Vec<_>>()
    });
    assert!(marked.contains(&true) && marked.contains(&false), "rows with and without: {marked:?}");
    let edges: Vec<(f32, f32)> = (0..6)
        .map(|ix| {
            let row: &'static str = Box::leak(format!("palette-item-{ix}").into_boxed_str());
            let title: &'static str = Box::leak(format!("palette-title-{ix}").into_boxed_str());
            let row = cx.debug_bounds(row).expect("a row");
            let title = cx.debug_bounds(title).expect("its title");
            (f32::from(row.origin.x), f32::from(title.origin.x))
        })
        .collect();
    let (row_x, title_x) = edges[0];
    assert!(title_x > row_x, "the icon slot comes first");
    for (ix, (r, t)) in edges.iter().enumerate() {
        assert!((r - row_x).abs() < 0.5 && (t - title_x).abs() < 0.5, "row {ix}: {edges:?}");
    }
}

/// The empty workspace offers the three ways to begin with their keys, read from the bindings,
/// and each does what its key does: a shell, an agent, the picker (at once, waiting on the
/// worker). The workers are listed with how they are doing.
#[gpui::test]
fn the_empty_workspace_begins_a_terminal_an_agent_or_a_window(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    for label in ["New terminal", "New agent", "Add a window or display", "studio, connected"] {
        assert!(tree.iter().any(|n| n.is("Button", Some(label))), "{label}: {tree:#?}");
    }
    assert_eq!(*strip::BEGIN_KEYS, ["⌘T".to_owned(), "⇧⌘T".to_owned(), "⌘O".to_owned()]);

    click(cx, "empty-terminal");
    assert!(
        matches!(
            fake.drain().as_slice(),
            [ClientMsg::OpenSession(OpenSession { command, .. })] if command.is_empty()
        ),
        "a shell"
    );
    click(cx, "empty-agent");
    assert!(
        matches!(
            fake.drain().as_slice(),
            [ClientMsg::OpenSession(OpenSession { command, .. })]
                if command == &[AGENT_COMMAND.to_owned()]
        ),
        "an agent"
    );
    click(cx, "empty-window");
    assert!(
        matches!(fake.drain().as_slice(), [ClientMsg::Screen(ScreenRequest::List)]),
        "the worker is asked for its windows"
    );
    assert!(cx.debug_bounds("picker-loading").is_some(), "the picker is up, waiting");
}

fn listing(title: &str) -> ScreenEvent {
    ScreenEvent::Listing {
        windows: vec![WindowInfo {
            id: WindowId(7),
            app: "Safari".to_owned(),
            bundle_id: None,
            title: title.to_owned(),
            x: 0.0,
            y: 0.0,
            w: 1200.0,
            h: 800.0,
            display: 1,
            on_screen: true,
        }],
        displays: vec![DisplayInfo { id: 1, w: 1728.0, h: 1117.0, scale: 2.0, hz: 120.0 }],
    }
}

/// ⌘O shows the picker while the worker lists its windows, and the listing fills that picker
/// rather than opening another. Dismissed before the listing comes, the picker stays gone.
#[gpui::test]
fn cmd_o_shows_the_picker_while_the_worker_lists_its_windows(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    fake.drain();
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    assert!(fake.drain().iter().any(|m| matches!(m, ClientMsg::Screen(ScreenRequest::List))));
    assert!(cx.debug_bounds("picker-loading").is_some(), "waiting");
    assert!(cx.debug_bounds("picker-session-0").is_some(), "the sessions are there already");
    let first = view.read_with(cx, |v, _| v.picker.as_ref().map(|(_, p)| p.entity_id()));
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| v.screen_event(key, listing("Rust docs"), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("picker-loading").is_none());
    assert!(cx.debug_bounds("picker-window-0").is_some(), "the window is listed");
    assert!(cx.debug_bounds("picker-display-0").is_some(), "and the display");
    let filled = view.read_with(cx, |v, _| v.picker.as_ref().map(|(_, p)| p.entity_id()));
    assert_eq!(first, filled, "the same picker, filled in");

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("picker").is_none());
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| v.screen_event(key, listing("late"), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("picker").is_none(), "a late listing opens nothing");
}

/// The overview's names are chrome: the same size however far the overview zooms out to fit
/// the workspaces. The place for a new workspace keeps its dashed outline with a plus and its
/// name inside.
#[gpui::test]
fn the_overview_labels_keep_their_size_at_any_zoom(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    three_shells(&view, cx, &fake);
    let overview = |cx: &mut VisualTestContext, new: &'static str| {
        cx.simulate_keystrokes("cmd-alt-o");
        cx.run_until_parked();
        let zoom = view.read_with(cx, |v, _| v.drawn_zoom);
        let name = cx.debug_bounds("overview-name-0").expect("the first workspace's name");
        let new = cx.debug_bounds(new).expect("the place for a new one");
        cx.simulate_keystrokes("cmd-alt-o");
        cx.run_until_parked();
        (zoom, f32::from(name.size.height), f32::from(new.size.height))
    };
    let (zoom, name, new) = overview(cx, "overview-name-1");
    // The focused tile goes down into a workspace of its own: three to fit, not two.
    cx.simulate_keystrokes("cmd-alt-shift-down");
    cx.run_until_parked();
    let (fitted, name_after, new_after) = overview(cx, "overview-name-2");
    assert!(fitted < zoom, "more workspaces, smaller: {zoom} then {fitted}");
    assert!(name > 0.0 && (name - new).abs() < 0.01, "one type size: {name} {new}");
    assert!((name - name_after).abs() < 0.01, "{name} then {name_after}");
    assert!((new - new_after).abs() < 0.01, "{new} then {new_after}");
}

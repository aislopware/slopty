//! The palette's sections, the picker's waiting row, the empty workspace and the
//! palette's labels, in the headless workspace.

use gpui::{AppContext as _, Modifiers};
use slopty_client::layout::Saved;
use slopty_core::{DisplayId, WindowId};
use slopty_proto::screen::{DisplayInfo, WindowInfo};

use super::super::actions::ScopeTo;
use super::*;
use crate::palette::PaletteRun;

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
    cx.dispatch_action(OpenPalette);
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_some(), "the palette is up");
}

/// The palette lists the tiles first, then the workers, then the commands, each group under
/// its heading (every head after the first a large step below the rows before it), with the
/// worker named on a tile's line once there are two of them, and a
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
    // Sections part by space: every head after the first stands a large step below the rows
    // before it, the first a small one below the field.
    let tall = |cx: &mut VisualTestContext, heading: &'static str| {
        let at = cx.debug_bounds(heading).expect("drawn");
        f32::from(at.size.height)
    };
    let spacing = Theme::default().spacing;
    let first = tall(cx, "palette-heading-tiles");
    for later in ["palette-heading-workers", "palette-heading-commands"] {
        let step = tall(cx, later) - first;
        assert!((step - (spacing.lg - spacing.sm)).abs() < 0.5, "{later} stands lower: {step}");
    }
    let lines = view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("open");
        palette
            .read(cx)
            .matches()
            .into_iter()
            .map(|l| (l.section, l.a11y_label(), l.status))
            .collect::<Vec<_>>()
    });
    let first = lines.first().expect("lines");
    assert_eq!(first.0, crate::palette::Section::Tiles, "{lines:#?}");
    assert!(first.1.contains("studio"), "the worker is named: {lines:#?}");
    assert_eq!(first.2, Some(slopty_kit::icons::Status::NeedsYou), "the agent is marked");
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
    let tree = cx.update(|window, _cx| slopty_kit::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Heading", Some("Machines"))), "{tree:#?}");
    assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("New terminal ⇧⌘T"))), "{tree:#?}");

    // "lap" names the laptop and its own commands, and no tile: none runs there.
    cx.simulate_keystrokes("l a p");
    cx.run_until_parked();
    assert!(top(cx, "palette-heading-tiles").is_none(), "an empty group hides its heading");
    assert!(top(cx, "palette-heading-workers").is_some());
    assert!(top(cx, "palette-heading-commands").is_some());

    // Only one command is left: one group, no heading at all.
    cx.simulate_keystrokes("backspace backspace backspace");
    cx.simulate_input("clipboard with laptop");
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette-item-0").is_some(), "the laptop's clipboard line");
    assert!(cx.debug_bounds("palette-item-1").is_none());
    for heading in ["palette-heading-tiles", "palette-heading-workers", "palette-heading-commands"]
    {
        assert!(top(cx, heading).is_none(), "{heading} over a lone group");
    }
    // ↩ runs what is shown: the line left.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
}

/// With no keyboard attached, as on a phone, the palette prints no chord that cannot be
/// pressed: the commands lose their keys. A worker's line keeps its
/// readout, which is not a chord. Esc is gone with the keys, so the field ends in Cancel on the
/// field's row and a scrim dims the work, and Cancel closes it.
#[gpui::test]
fn without_a_keyboard_the_palette_prints_no_chords(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _studio = connect(&view, cx, 1, "studio");
    let chords = |cx: &mut VisualTestContext| {
        (1..80).filter(|ix| cx.debug_bounds(format!("palette-keys-{ix}").leak()).is_some()).count()
    };
    open_palette(cx);
    assert!(chords(cx) > 0, "a Mac prints its chords");
    assert!(cx.debug_bounds("palette-cancel").is_none(), "Esc closes it: no Cancel");
    assert!(cx.debug_bounds("palette-scrim").is_none(), "and it floats undimmed");
    cx.simulate_keystrokes("escape");
    view.update(cx, |v, cx| v.set_hardware_keyboard(false, cx));
    open_palette(cx);
    assert_eq!(chords(cx), 0, "no keyboard, no chords");
    assert!(cx.debug_bounds("palette-scrim").is_some(), "a scrim to tap");
    let field = cx.debug_bounds("palette").expect("the palette");
    let cancel = cx.debug_bounds("palette-cancel").expect("Cancel on glass");
    assert!(cancel.top() - field.top() < px(60.0), "on the field's row: {cancel:?} {field:?}");
    assert!(cancel.center().x > field.center().x, "at its trailing end: {cancel:?} {field:?}");
    click(cx, "palette-cancel");
    assert!(view.read_with(cx, |v, _| v.palette.is_none()), "Cancel closes it");
}

/// Every line's title starts on one edge: a tile's or a worker's kind icon sits in a fixed
/// slot, whose mark a status takes over, and a command, with no icon, keeps the slot empty, so
/// the list reads down one left edge.
#[gpui::test]
fn the_icon_slot_keeps_every_title_on_one_edge(cx: &mut TestAppContext) {
    use crate::palette::Section::{Commands, Tiles, Workers};

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _laptop = connect(&view, cx, 2, "laptop");
    let waiting = SessionId::new();
    opens(&view, cx, &studio, waiting, studio.me, 1);
    opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(waiting), cx));
    cx.run_until_parked();
    open_palette(cx);
    let lines = view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("open");
        palette
            .read(cx)
            .matches()
            .iter()
            .map(|l| (l.section, l.status.is_some()))
            .collect::<Vec<_>>()
    });
    let marked: Vec<bool> = lines.iter().map(|l| l.1).collect();
    assert!(marked.contains(&true) && marked.contains(&false), "rows with and without: {marked:?}");
    let edges: Vec<(crate::palette::Section, f32, f32)> = lines
        .iter()
        .enumerate()
        .map(|(ix, (section, _))| {
            let row: &'static str = Box::leak(format!("palette-item-{ix}").into_boxed_str());
            let title: &'static str = Box::leak(format!("palette-title-{ix}").into_boxed_str());
            let row = cx.debug_bounds(row).expect("a row");
            let title = cx.debug_bounds(title).expect("its title");
            (*section, f32::from(row.origin.x), f32::from(title.origin.x))
        })
        .collect();
    let edge = |wanted: crate::palette::Section| {
        let xs: Vec<(f32, f32)> =
            edges.iter().filter(|e| e.0 == wanted).map(|e| (e.1, e.2)).collect();
        let first = *xs.first().unwrap_or_else(|| panic!("{wanted:?} rows: {edges:?}"));
        for (r, t) in &xs {
            assert!((r - first.0).abs() < 0.5 && (t - first.1).abs() < 0.5, "{wanted:?}: {xs:?}");
        }
        first
    };
    let (row_x, tile_x) = edge(Tiles);
    assert!(tile_x > row_x, "the icon slot comes first");
    assert!((edge(Workers).1 - tile_x).abs() < 0.5, "a worker's title on a tile's edge");
    let (command_row, command_x) = edge(Commands);
    assert!((command_row - row_x).abs() < 0.5, "a command's row on the same edge");
    assert!((command_x - tile_x).abs() < 0.5, "an empty slot keeps a command's title on it");
}

/// The empty workspace asks what to work on over a composer, which takes the keyboard: what is
/// written there and sent starts a goal's orchestrator at once, as "New goal…" does, in the
/// machine's latest repository; with none known, the sheet takes the goal and asks where. A
/// shell and a window stay one press away under it, with no keys
/// on them, and each does what its key does. Nothing else stands on the page.
#[gpui::test]
fn the_empty_workspace_asks_what_to_work_on(cx: &mut TestAppContext) {
    use slopty_proto::server::InstalledAgent;
    use slopty_proto::thread::AgentId;
    use slopty_proto::thread::wire::ThreadRequest;

    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let key = fake.key;
    let claude = InstalledAgent {
        agent: AgentId::named(AgentId::CLAUDE_CODE),
        version: "1.0".to_owned(),
        offers: slopty_proto::thread::Offers::default(),
        managed_hooks_off: false,
    };
    let caps = WorkerCaps { agents: vec![claude], ..healthy() };
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(key, caps, cx);
        v.threads_linked(key, cx);
    });
    cx.run_until_parked();
    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| slopty_kit::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Heading", Some(EMPTY_QUESTION))), "{tree:#?}");
    for label in ["New terminal", "Add a window or display"] {
        assert!(tree.iter().any(|n| n.is("Button", Some(label))), "{label}: {tree:#?}");
    }
    for gone in ["empty-agent", "empty-recent", "empty-workers", "empty-worker-0", "empty-mark"] {
        assert!(cx.debug_bounds(gone).is_none(), "{gone}: no launcher, lists or mark");
    }
    for gone in ["title-tabs-forward", "title-tabs-back"] {
        assert!(cx.debug_bounds(gone).is_none(), "{gone}: no tabs, nothing to scroll to");
    }
    let page = cx.debug_bounds("empty-workspace").expect("the page");
    let goal = cx.debug_bounds("empty-goal").expect("the composer");
    assert!(page.bottom() - goal.bottom() < px(Theme::default().spacing.md), "at the foot");

    fake.drain();
    cx.simulate_input("Fix the login redirect");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let started: Vec<_> = fake
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some(start),
            _ => None,
        })
        .collect();
    // No repository known to work in: nothing starts in the home folder, and the sheet,
    // holding the goal, asks where.
    assert!(started.is_empty(), "nothing starts: {started:?}");
    let typed = view.read_with(cx, WorkspaceView::goal_sheet_typed).expect("the sheet");
    assert_eq!(typed.goal, "Fix the login redirect", "holding the goal");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("goal-sheet").is_none(), "put away");

    click(cx, "empty-terminal");
    assert!(
        matches!(
            fake.drain().as_slice(),
            [ClientMsg::OpenSession { spec: OpenSession { command, .. }, .. }] if command.is_empty()
        ),
        "a shell"
    );
    click(cx, "empty-window");
    assert!(
        matches!(fake.drain().as_slice(), [ClientMsg::Screen(ScreenRequest::List)]),
        "the worker is asked for its windows"
    );
    assert!(cx.debug_bounds("picker-loading").is_some(), "the picker is up, waiting");
}

/// The empty workspace is a thread with no turns (`.research/design-polish-2026-10-12.md`
/// §4.9): the question, the line saying where a goal goes, the heads and the doors' glyphs
/// stand on one edge in a column of 512 whose top is 30 % down the launcher, and the goal's
/// composer waits at the pane's foot, where a thread keeps it.
#[gpui::test]
fn the_empty_page_has_one_edge(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    view.update_in(cx, |v, _w, cx| v.threads_linked(fake.key, cx));
    cx.run_until_parked();
    let at = |cx: &mut VisualTestContext, s: &'static str| {
        cx.debug_bounds(s).unwrap_or_else(|| panic!("{s} is not drawn"))
    };
    let theme = Theme::default();
    let question = at(cx, "empty-question");
    let edge = question.left();
    for s in ["empty-goal-foot", "empty-start"] {
        let b = at(cx, s);
        assert!((b.left() - edge).abs() < px(0.5), "{s} on the edge: {b:?} {question:?}");
    }
    for door in ["empty-terminal", "empty-window", "empty-new-goal"] {
        let glyph = at(cx, door).left() + px(theme.spacing.inset());
        assert!((glyph - edge).abs() < px(0.5), "{door}'s glyph hangs on the edge");
    }
    let (page, goal, column) =
        (at(cx, "empty-workspace"), at(cx, "empty-goal"), at(cx, "empty-column"));
    assert!(column.size.width <= px(512.0 + 0.5), "{column:?}");
    let launcher = goal.top() - page.top();
    let top = column.top() - page.top();
    assert!((top - launcher * 0.3).abs() < px(1.0), "30 % down: {top:?} of {launcher:?}");
    assert!(page.bottom() - goal.bottom() < px(theme.spacing.md), "the composer at the foot");
    assert!(goal.top() > at(cx, "empty-new-goal").bottom(), "under the launcher");
}

/// A project on show with nothing open in it asks what to work on there, by its name; with no
/// project on show, or a machine's own work, the question names none.
#[gpui::test]
fn an_empty_project_asks_what_to_work_on_in_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    cx.update(|window, _cx| window.set_a11y_active(true));
    let heading = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| slopty_kit::a11y::tree(window));
        tree.iter().find(|n| n.role == "Heading").and_then(|n| n.label.clone())
    };
    assert_eq!(heading(cx).as_deref(), Some(EMPTY_QUESTION), "no project on show");
    let studio = slopty_client::groups::GroupKey::machine(fake.key);
    view.update_in(cx, |v, _w, cx| v.layout_action(cx, |l| l.show_project(&studio)));
    assert_eq!(heading(cx).as_deref(), Some(EMPTY_QUESTION), "a machine is no project");
    let atlas = slopty_client::groups::GroupKey::new("project", "atlas");
    view.update_in(cx, |v, _w, cx| {
        v.layout_action(cx, |l| {
            l.show_project(&atlas);
            l.set_name(&atlas, Some("Atlas".to_owned()));
        });
    });
    assert_eq!(heading(cx).as_deref(), Some("What should we work on in Atlas?"), "named");
}

/// The line under the question says where ↵ sends the goal (the machine, the folder, the
/// agent), so nothing starts somewhere unseen; pressed, it opens "New goal…"'s sheet holding
/// what was typed, and the words move there. The doors print no keys: keys are the palette's.
#[gpui::test]
fn the_empty_workspaces_composer_says_where_the_goal_goes(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    view.update_in(cx, |v, _w, cx| v.threads_linked(fake.key, cx));
    cx.run_until_parked();
    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.run_until_parked();
    // No repository known yet: the machine and the agent, and the sheet asks for the folder.
    let place = "studio · Claude Code";
    let tree = cx.update(|window, _cx| slopty_kit::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Button", Some(place))), "where it goes: {tree:#?}");
    let (foot, goal) = (cx.debug_bounds("empty-goal-foot"), cx.debug_bounds("empty-goal"));
    let (Some(foot), Some(goal)) = (foot, goal) else { panic!("the place and the composer") };
    let question = cx.debug_bounds("empty-question").expect("the question");
    assert!(foot.top() >= question.bottom() && foot.bottom() < goal.top(), "under the question");
    for keys in ["⇧⌘T", "⌘O"] {
        assert!(
            !tree.iter().any(|n| n.label.as_deref().is_some_and(|l| l.contains(keys))),
            "{keys}"
        );
    }

    cx.simulate_input("Fix the login redirect");
    cx.simulate_click(foot.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("goal-sheet").is_some(), "the sheet, to change it");
    let typed = view.read_with(cx, WorkspaceView::goal_sheet_typed).expect("the sheet");
    assert_eq!(typed.goal, "Fix the login redirect", "holding what was typed");
    let left = view
        .read_with(cx, |v, cx| v.empty_goal.as_ref().map(|(g, _)| g.read(cx).value().to_string()));
    assert_eq!(left.as_deref(), Some(""), "the words moved");
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
            display: DisplayId(1),
            on_screen: true,
        }],
        displays: vec![DisplayInfo {
            id: DisplayId(1),
            w: 1728.0,
            h: 1117.0,
            scale: 2.0,
            hz: 120.0,
        }],
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

/// The picker is left the way the palette is: Esc with a keyboard, and on glass without one a
/// Cancel at the end of its field and a dim to tap around it.
#[gpui::test]
fn without_a_keyboard_the_picker_ends_in_cancel(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    assert!(cx.debug_bounds("picker").is_some(), "the picker is up");
    assert!(cx.debug_bounds("picker-cancel").is_none(), "Esc closes it: no Cancel");
    assert!(cx.debug_bounds("picker-scrim").is_none(), "and it floats undimmed");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("picker").is_none());

    view.update(cx, |v, cx| v.set_hardware_keyboard(false, cx));
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    assert!(cx.debug_bounds("picker-scrim").is_some(), "a dim to tap");
    let field = cx.debug_bounds("picker").expect("the picker");
    let cancel = cx.debug_bounds("picker-cancel").expect("Cancel on glass");
    assert!(cancel.top() - field.top() < px(60.0), "on the field's row: {cancel:?} {field:?}");
    assert!(cancel.center().x > field.center().x, "at its trailing end: {cancel:?} {field:?}");
    click(cx, "picker-cancel");
    assert!(view.read_with(cx, |v, _| v.picker.is_none()), "Cancel closes it");
    assert!(cx.debug_bounds("picker").is_none());
}

/// On a desktop the palette hangs near the top ([`crate::palette::LIST_TOP`] down), at most
/// [`crate::palette::LIST_WIDTH`] wide, and takes at most three fifths of the window's height
/// under its ceiling, shorter still when it lists less. On a phone it is a sheet from the bottom,
/// the window's width, from under the status bar down to the keyboard, its field heading it. On
/// both a fade covers the list's end while more runs on below it.
#[gpui::test]
fn the_palette_hangs_near_the_top_and_is_a_sheet_on_a_phone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    // One shell, so the brief list (its tile, its machine, a few commands) stays short of the
    // ceiling a query's matches reach.
    opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    // Where the sheet lands, not its rise into place.
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    open_palette(cx);
    let (w, h) = VIEWPORT;
    let brief = cx.debug_bounds("palette").expect("drawn");
    let near = crate::palette::LIST_TOP;
    assert!((f32::from(brief.top()) - near).abs() < 0.5, "near the top: {brief:?}");
    let wide = crate::palette::LIST_WIDTH;
    assert!((f32::from(brief.size.width) - wide).abs() < 0.5, "Delta's width: {brief:?}");
    cx.simulate_input("e");
    cx.run_until_parked();
    let full = cx.debug_bounds("palette").expect("drawn");
    let ceiling = slopty_kit::kit::Overlay::List.bounds().1.min(h * 0.6);
    assert!((f32::from(full.size.height) - ceiling).abs() < 0.5, "at its ceiling: {full:?}");
    assert!(brief.size.height < full.size.height, "the brief list is shorter: {brief:?}");
    // The row the ceiling cuts fades out above the foot rather than ending on its hairline.
    cx.update(Window::simulate_next_frame);
    cx.run_until_parked();
    assert!(more_below(cx), "a cut row fades on a desktop too");
    assert!(w > 700.0, "a desktop window");

    // The fade reads the list's extent a frame late: the tests have no frame loop to run it.
    let next_frame = |cx: &mut VisualTestContext| {
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
    };
    let (phone_w, short) = (390.0, 420.0);
    cx.simulate_resize(size(px(phone_w), px(short)));
    cx.run_until_parked();
    next_frame(cx);
    let palette = cx.debug_bounds("palette").expect("drawn");
    let gap = Theme::default().spacing.sm;
    assert!((f32::from(palette.top()) - gap).abs() < 0.5, "a gap under the top: {palette:?}");
    assert!((f32::from(palette.bottom()) - short).abs() < 0.5, "to the keyboard: {palette:?}");
    let field = cx.debug_bounds("palette-field").expect("the field");
    let list = cx.debug_bounds("palette-list").expect("the list");
    assert!(list.top() >= field.bottom(), "the field over what it finds: {field:?} {list:?}");
    // Under the sheet's one line along its top.
    let under = f32::from(field.top() - palette.top());
    assert!((0.0..1.5).contains(&under), "at the head: {field:?}");
    assert!(
        f32::from(palette.left()).abs() < 0.5 && (f32::from(palette.right()) - phone_w).abs() < 0.5,
        "the window's width: {palette:?}"
    );
    assert!(more_below(cx), "more below: the fade says so");
    // ↑ from the first line wraps to the last, and the list scrolls to its end.
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    next_frame(cx);
    assert!(!more_below(cx), "nothing more below");
}

/// Whether the palette's rows fade out at their foot.
fn more_below(cx: &mut VisualTestContext) -> bool {
    let rows = cx.debug_bounds("palette-rows").expect("the palette's rows");
    cx.update(|window, _| slopty_kit::retained::faded_edges(window, rows)).bottom
}

/// ↓ past the lines in view scrolls the palette's list with the selection, and a query that
/// moves the selection back to the first line brings that line back into view.
#[gpui::test]
fn the_palette_scrolls_to_the_selected_line(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    // A shell has the keyboard, so the list holds its commands too and runs long.
    opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    // Where the lines land, not the sheet rising into place under them.
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    open_palette(cx);
    // Every command with an e in it: past the brief list an empty field shows.
    cx.simulate_input("e");
    cx.run_until_parked();
    // A line far from the view is not laid out at all: the list draws what it shows.
    let inside = |cx: &mut VisualTestContext, line: &'static str| {
        let list = cx.debug_bounds("palette-list").expect("the list is drawn");
        cx.debug_bounds(line).is_some_and(|l| l.top() >= list.top() && l.bottom() <= list.bottom())
    };
    let far = "palette-item-30";
    assert!(!inside(cx, far), "far down the list at first");
    for _ in 0..30 {
        cx.simulate_keystrokes("down");
    }
    cx.run_until_parked();
    assert!(inside(cx, far), "stepped to, and scrolled to");
    assert!(!inside(cx, "palette-item-0"), "the top scrolled away");
    cx.simulate_input("n");
    cx.run_until_parked();
    assert!(inside(cx, "palette-item-0"), "a new query selects the first line, in view");
}

/// An agent's line is found by the last line its agent wrote, as its thread's row says it, as
/// well as by its title; a word in neither finds nothing.
#[gpui::test]
fn the_palette_finds_an_agent_by_what_its_thread_last_said(cx: &mut TestAppContext) {
    use slopty_proto::thread::Cursor;
    use slopty_proto::thread::wire::TableFrame;

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let mut row = state.row(WallMs::ZERO);
    row.title = "Fix the flaky test".to_owned();
    row.last_line = Some("The **correct step** was to pin the clock".to_owned());
    let table = TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows: vec![row] };
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(studio.key, cx);
        v.thread_table(studio.key, &table, cx);
    });
    cx.run_until_parked();
    let found = |cx: &mut VisualTestContext, query: &str| {
        view.update(cx, |v, cx| {
            let lines = v.palette_lines(cx);
            crate::palette::filter(query, &lines)
                .iter()
                .any(|line| matches!(line.run, PaletteRun::Session(s) if s == session))
        })
    };
    assert!(found(cx, "flaky test"), "by its title");
    assert!(found(cx, "pin the clock"), "by what it last said");
    assert!(!found(cx, "login redirect"), "by neither");
}

/// The navigator lists by project and nothing else: the palette offers no other grouping.
#[gpui::test]
fn the_navigator_offers_no_other_grouping(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _tile = nav_rows::in_repo(&view, cx, &studio, 1, ("/w/a", "/w/a", "main"));
    let lines = view.update(cx, |v, cx| v.palette_lines(cx));
    let grouping: Vec<_> = lines.iter().filter(|l| l.label.starts_with("Group the")).collect();
    assert!(grouping.is_empty(), "{:?}", grouping.iter().map(|l| &l.label).collect::<Vec<_>>());
}

/// The palette offers what the focus can do, as the dispatch tree last drawn says: nothing
/// about a tile while none has the focus, a shell's own commands while its terminal has the
/// keyboard, and never a page's, a file's, a remote window's or an agent's there. What the
/// workspace does whatever has the focus is always there. A pick nothing answers once it
/// runs (its tile went while the palette was open) is said, never dropped without a word.
#[gpui::test]
fn the_palette_offers_what_the_focus_can_do(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let offered = |cx: &mut VisualTestContext| -> Vec<String> {
        let lines = view.update_in(cx, |v, window, cx| v.offered_lines(window, cx));
        lines.into_iter().map(|l| l.label).collect()
    };
    let has = |labels: &[String], label: &str| labels.iter().any(|l| l == label);
    let always = ["New terminal", "New note", "Previous project", "Search in files"];
    let of_a_tile = ["Close tile", "Name this tile", "Zoom pane", "Move tile left"];
    let elsewhere = [
        "Page back",
        "Reload page",
        "Save file",
        SAVE_A_COPY,
        "Mute sound",
        "Stop the agent",
        "Switch thread, terminal or board",
        "Enclosing folder",
        "Undo close",
        "Open last offered page",
    ];

    let empty = offered(cx);
    for label in always {
        assert!(has(&empty, label), "{label} is always offered: {empty:?}");
    }
    for label in of_a_tile.iter().chain(&elsewhere).chain(&["Clear the screen and history"]) {
        assert!(!has(&empty, label), "{label} with no tile: {empty:?}");
    }

    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, fake.me, 1);
    assert!(terminal_focused(&view, cx, session));
    let shell = offered(cx);
    for label in always.iter().chain(&of_a_tile) {
        assert!(has(&shell, label), "{label} with a shell: {shell:?}");
    }
    for label in ["Clear the screen and history", "Find in terminal, file or thread"] {
        assert!(has(&shell, label), "the terminal's own {label}: {shell:?}");
    }
    for label in elsewhere {
        assert!(!has(&shell, label), "{label} is not a shell's: {shell:?}");
    }

    cx.dispatch_action(OpenPalette);
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| {
        let by = ClientId::new();
        let gone = ItemSync::Delta { version: 2, by, op: ItemOp::Remove(tile.item) };
        v.apply_sync(fake.key, gone, cx);
    });
    cx.run_until_parked();
    // The line as the palette picks it, its tile gone meanwhile.
    let palette = view.read_with(cx, |v, _| v.palette.clone()).expect("open");
    palette.update(cx, |_p, cx| {
        let rename = Box::new(RenameItem);
        cx.emit(crate::palette::PaletteEvent::Run(PaletteRun::Action(rename)));
    });
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("Name this tile does not apply here"));
}

/// A shell in `repo` on `fake`, opened by this client (`mine`) or by another.
pub(super) fn shell_in(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    version: u64,
    repo: &str,
    mine: bool,
) -> (SessionId, TileRef) {
    let session = SessionId::new();
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::Terminal { session },
        name: None,
        facts: BTreeMap::new(),
    };
    let tile = TileRef { worker: fake.key, item: item.id };
    let (key, by) = (fake.key, if mine { fake.me } else { ClientId::new() });
    let summary = SessionSummary {
        repo: Some(repo.to_owned()),
        branch: Some("main".to_owned()),
        ..summary(session, Some(repo))
    };
    view.update_in(cx, |v, _window, cx| {
        v.session_opened(key, summary, cx);
        v.apply_sync(key, ItemSync::Delta { version, by, op: ItemOp::Add(item) }, cx);
    });
    cx.run_until_parked();
    (session, tile)
}

/// The palette lists each project first, ↩ going to the workspace that last held its tiles and
/// to its first tile there; a tile of it that another client opens lands in that workspace too.
#[gpui::test]
fn a_project_line_goes_to_its_workspace(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (_, _atlas) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    let (_, _atlas_two) = shell_in(&view, cx, &studio, 2, "/w/atlas", true);
    let (_, site) = shell_in(&view, cx, &studio, 3, "/w/site", true);
    let (_, docs) = shell_in(&view, cx, &studio, 4, "/w/docs", true);
    let names = |cx: &mut VisualTestContext| -> Vec<String> {
        view.read_with(cx, |v, _| v.project_rows().into_iter().map(|l| l.label).collect())
    };
    // Each once (atlas's two shells in a row are one visit): a tie goes by name.
    assert_eq!(names(cx), ["atlas", "docs", "site"]);
    for tile in [site, docs, site] {
        view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
        cx.run_until_parked();
    }
    assert_eq!(names(cx), ["site", "docs", "atlas"], "site three times, docs twice, atlas once");
    let saved = view.read_with(cx, |v, _| v.to_save().frecency);
    let restored = cx.update(|_w, cx| {
        cx.new(|cx| {
            let saved = Saved { frecency: saved, ..Default::default() };
            WorkspaceView::new(Theme::default(), Some(saved), cx)
        })
    });
    let kept = restored.read_with(cx, |v, _| v.frecency.clone());
    assert_eq!(kept, view.read_with(cx, |v, _| v.frecency.clone()), "kept with the layout");
}

/// "Scope to atlas" narrows the navigator and its attention sections alike to atlas; it leads
/// the filter as a token, the open field taking the row from New agent and New goal, the bell
/// keeps it, and Esc lets go of it.
#[gpui::test]
fn a_scope_filters_the_navigator_and_its_attention_sections_alike(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (atlas_agent, atlas) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    let (site_agent, site) = shell_in(&view, cx, &studio, 2, "/w/site", true);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(blocked(atlas_agent), cx);
        v.agent_event(blocked(site_agent), cx);
    });
    cx.run_until_parked();
    let row = |session: SessionId| -> &'static str {
        Box::leak(format!("nav-waiting-{session}").into_boxed_str())
    };
    assert!(cx.debug_bounds(row(site_agent)).is_some(), "both wait");
    let key = view.read_with(cx, |v, _| v.project_groups().group_of(atlas).map(|g| g.key.clone()));
    let scope = ScopeTo { project: key };
    view.update_in(cx, |v, w, cx| v.scope_to(&scope, w, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("nav-scope").is_some(), "the scope leads the filter");
    let input = cx.debug_bounds("nav-filter").expect("its field");
    assert!(f32::from(input.size.width) >= 24.0, "room to type in: {input:?}");
    assert!(cx.debug_bounds("nav-new-goal").is_none(), "the open field takes the starts' room");
    let listed = view.read_with(cx, |v, _| v.navigator_tiles());
    assert_eq!(listed, [atlas], "only atlas's rows");
    cx.simulate_keystrokes("cmd-shift-u");
    cx.run_until_parked();
    assert!(cx.debug_bounds("nav-scope").is_some(), "the bell keeps the scope");
    assert!(cx.debug_bounds(row(atlas_agent)).is_some(), "atlas's agent under Needs you");
    assert!(cx.debug_bounds(row(site_agent)).is_none(), "and not site's");

    click(cx, "nav-filter");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("nav-scope").is_none(), "Esc lets go of it");
    let listed = view.read_with(cx, |v, _| v.navigator_tiles());
    assert!(listed.contains(&site), "every row is back: {listed:?}");
    assert!(cx.debug_bounds(row(site_agent)).is_some(), "and site's agent with them");
}

/// The palette splits three ways, `MonoCode`'s. ⌘K searches everything and, before anything is
/// typed, lists the threads worked in lately that no tile shows, newest first (R17). ⌘⇧P opens
/// the commands alone with an empty field, no `>` to type past (Zed's command palette), and
/// Esc on it closes it. ⌘P searches the files alone: the ones open in tiles at once, and what is
/// typed asked of the machine's files.
#[gpui::test]
fn the_palette_splits_into_everything_commands_and_files(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::{TableFrame, ThreadRow};
    use slopty_proto::thread::{Cursor, ThreadId};

    use crate::palette::Section;

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let _notes = arrives(&view, cx, &studio, ItemKind::File { path: "/w/notes.txt".into() }, 2);
    let row = |title: &str, at: u64| -> ThreadRow {
        let mut state = crate::conversation::thread::fixtures::empty();
        state.meta.title = title.to_owned();
        state.meta.cwd = "/w/atlas".to_owned();
        state.row(WallMs::from_millis(at))
    };
    let rows = vec![row("older work", 1_000), row("newer work", 2_000)];
    let ids: Vec<ThreadId> = rows.iter().map(|r| r.id).collect();
    let key = studio.key;
    let table = TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows };
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        v.thread_table(key, &table, cx);
    });
    cx.run_until_parked();
    let shown = |cx: &mut VisualTestContext| -> Vec<(Section, String)> {
        view.read_with(cx, |v, cx| {
            let palette = v.palette.clone().expect("open");
            palette.read(cx).matches().into_iter().map(|l| (l.section, l.label.clone())).collect()
        })
    };

    open_palette(cx);
    let recent: Vec<String> = shown(cx)
        .into_iter()
        .filter(|(section, _)| *section == Section::Recent)
        .map(|(_, label)| label)
        .collect();
    assert_eq!(recent, ["newer work", "older work"], "newest first");
    assert!(cx.debug_bounds("palette-heading-recent-threads").is_some(), "under their heading");
    let first_recent = view.read_with(cx, |v, cx| {
        let palette = v.palette.clone().expect("open");
        palette.read(cx).matches().into_iter().find_map(|l| match l.run {
            PaletteRun::Thread { thread, turn: None } => Some(thread),
            _ => None,
        })
    });
    assert_eq!(first_recent, Some(ids[1]), "↩ opens it where it stands");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    cx.dispatch_action(OpenCommands);
    cx.run_until_parked();
    let field = view.read_with(cx, |v, cx| {
        v.palette.clone().map(|p| p.read(cx).input_value(cx)).unwrap_or_default()
    });
    assert_eq!(field, "", "the commands alone, no prompt in the field");
    let lines = shown(cx);
    assert!(lines.iter().all(|(section, _)| *section == Section::Commands), "{lines:?}");
    assert!(lines.len() > crate::palette::RECENT_COMMANDS, "every command: {}", lines.len());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.palette.is_none()), "Esc on the empty field closes");

    studio.drain();
    cx.dispatch_action(OpenFile);
    cx.run_until_parked();
    let lines = shown(cx);
    assert_eq!(lines.len(), 1, "the file open in a tile, nothing else: {lines:?}");
    assert!(lines[0].1.contains("notes.txt"), "{lines:?}");
    cx.simulate_input("ma");
    cx.run_until_parked();
    let asked = studio
        .drain()
        .into_iter()
        .any(|m| matches!(m, ClientMsg::FindFiles { query, .. } if query == "ma"));
    assert!(asked, "what is typed is asked of the machine's files");
    assert!(shown(cx).iter().all(|(s, _)| *s != Section::Commands), "no command among them");
}

/// The workspace's palette has Delta's scope row under its field: All, Threads, Commands and
/// Files, ⌘1 to ⌘4 or a press choosing one, each listing its own lines alone
/// (`.research/delta-signed-in-2026-10-12.md` §2.7).
#[gpui::test]
fn the_palette_scope_row_lists_one_kind_at_a_time(cx: &mut TestAppContext) {
    use crate::palette::{Scope, Section};

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let shown = |cx: &mut VisualTestContext| -> Vec<Section> {
        view.read_with(cx, |v, cx| {
            let palette = v.palette.clone().expect("open");
            palette.read(cx).matches().into_iter().map(|l| l.section).collect()
        })
    };
    let scope = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| v.palette.clone().map(|p| p.read(cx).scope()))
    };
    open_palette(cx);
    let field = cx.debug_bounds("palette-field").expect("the field");
    let row = cx.debug_bounds("palette-scopes").expect("the scope row");
    assert!(row.top() >= field.bottom() - px(0.5), "under the field: {field:?} {row:?}");
    let title = Theme::default().density.title;
    assert!((f32::from(row.size.height) - title).abs() < 0.5, "Delta's 34 pt row: {row:?}");
    assert_eq!(scope(cx), Some(Scope::All));
    let all = shown(cx);
    assert!(all.contains(&Section::Tiles) && all.contains(&Section::Commands), "{all:?}");

    cx.simulate_keystrokes("cmd-3");
    cx.run_until_parked();
    assert_eq!(scope(cx), Some(Scope::Commands));
    let commands = shown(cx);
    assert!(commands.iter().all(|s| *s == Section::Commands), "{commands:?}");
    assert!(commands.len() > crate::palette::RECENT_COMMANDS, "every command: {commands:?}");

    click(cx, "palette-scope-threads");
    assert_eq!(scope(cx), Some(Scope::Threads));
    let threads = shown(cx);
    assert!(!threads.is_empty(), "the shell's tile");
    assert!(threads.iter().all(|s| Scope::Threads.holds(*s)), "{threads:?}");

    click(cx, "palette-scope-all");
    assert_eq!(shown(cx), all, "All lists everything again");
}

/// No two lines of the palette run the same action or read the same: each thing has one door
/// there (`.research/orchestrator-first-2026-10-10.md`, item 6). Checked with nothing open and
/// with a shell focused on one of two machines, where the most lines are offered.
#[gpui::test]
fn no_palette_line_repeats_another(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _laptop = connect(&view, cx, 2, "laptop");
    let check = |cx: &mut VisualTestContext, when: &str| {
        let lines = view.update_in(cx, |v, window, cx| v.offered_lines(window, cx));
        let mut labels = HashSet::new();
        for (ix, line) in lines.iter().enumerate() {
            assert!(labels.insert(line.label.as_str()), "{when}: {:?} twice", line.label);
            let PaletteRun::Action(action) = &line.run else { continue };
            let again = lines.iter().skip(ix.saturating_add(1)).find(|other| {
                matches!(&other.run, PaletteRun::Action(o) if o.partial_eq(action.as_ref()))
            });
            assert!(
                again.is_none(),
                "{when}: {:?} and {:?} run one action",
                line.label,
                again.map(|o| &o.label)
            );
        }
    };
    check(cx, "nothing open");
    opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    check(cx, "a shell");
}

/// The palette is drawn whole in the frame that shows it: as it opens, as a query narrows it
/// and as one widens it again, the frame is the one a frame from scratch draws
/// (`slopty_kit::retained::stale`), its plate as tall as its rows.
#[gpui::test]
fn the_palette_follows_its_query_in_the_frame_it_shows(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let fresh = |cx: &mut VisualTestContext, step: &str| {
        let stale = cx.update(|window, cx| slopty_kit::retained::stale(window, cx, 12));
        assert!(stale.is_none(), "{step}: {}", stale.unwrap_or_default());
    };
    cx.dispatch_action(OpenCommands);
    cx.run_until_parked();
    fresh(cx, "open");
    cx.simulate_input("new agent");
    cx.run_until_parked();
    fresh(cx, "narrowed");
    cx.simulate_keystrokes("backspace backspace backspace backspace backspace");
    cx.run_until_parked();
    fresh(cx, "widened");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    fresh(cx, "closed");
}

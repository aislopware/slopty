//! The palette's sections, the picker's waiting row, the empty workspace and the overview's
//! labels, in the headless workspace.

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
            .matches()
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
    assert!(tree.iter().any(|n| n.is("Heading", Some("Machines"))), "{tree:#?}");
    assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("New terminal ⌘T"))), "{tree:#?}");

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

/// The palette's foot names its keys in sentence case, as the key bar does: ↑↓ move and Esc
/// closes, and ↩ says what it does with the line selected (goes to a worker, runs a command). It
/// sits under the list, across the dialog, and reads as one line.
#[gpui::test]
fn the_palette_foot_names_its_keys(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _studio = connect(&view, cx, 1, "studio");
    cx.update(|window, _cx| window.set_a11y_active(true));
    open_palette(cx);
    let legend = |cx: &mut VisualTestContext| {
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        tree.into_iter()
            .filter(|n| n.role == "Label")
            .filter_map(|n| n.label)
            .find(|l| l.starts_with("↑↓"))
    };
    assert_eq!(legend(cx).as_deref(), Some("↑↓ Move · Esc Close · ↩ Go to"), "on the worker");
    cx.simulate_keystrokes("down");
    assert_eq!(legend(cx).as_deref(), Some("↑↓ Move · Esc Close · ↩ Run"), "on a command");
    let (dialog, legend, list) = (
        cx.debug_bounds("palette").expect("the dialog"),
        cx.debug_bounds("palette-legend").expect("the foot"),
        cx.debug_bounds("palette-item-0").expect("a line"),
    );
    assert!(legend.top() > list.bottom(), "under the list: {legend:?} {list:?}");
    assert!((f32::from(dialog.bottom()) - f32::from(legend.bottom())).abs() < 2.0, "at the foot");
}

/// With no keyboard attached, as on a phone, the palette prints no chord that cannot be
/// pressed: the commands lose their keys and the foot its legend. A worker's line keeps its
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
    assert!(cx.debug_bounds("palette-legend").is_some(), "and the legend");
    assert!(cx.debug_bounds("palette-cancel").is_none(), "Esc closes it: no Cancel");
    assert!(cx.debug_bounds("palette-scrim").is_none(), "and it floats undimmed");
    cx.simulate_keystrokes("escape");
    view.update(cx, |v, cx| v.set_hardware_keyboard(false, cx));
    open_palette(cx);
    assert_eq!(chords(cx), 0, "no keyboard, no chords");
    assert!(cx.debug_bounds("palette-legend").is_none(), "nor a legend");
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

/// The empty workspace asks what an agent should do and takes the keyboard to ask it: what is
/// typed starts the agent's thread with it as its first prompt, in the machine's latest place. A
/// shell and a window are the quieter ways in, with their keys read from the bindings, and each
/// does what its key does. The workers are listed with how they are doing.
#[gpui::test]
fn the_empty_workspace_asks_what_an_agent_should_do(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    for label in ["New terminal", "Add a window or display", "studio", "On studio", "In ~"] {
        assert!(tree.iter().any(|n| n.is("Button", Some(label))), "{label}: {tree:#?}");
    }
    assert!(!tree.iter().any(|n| n.is("Button", Some("New agent"))), "the question is the way");
    assert_eq!(strip::begin_keys(), ["⌘T".to_owned(), "⌘O".to_owned()]);

    cx.simulate_input("Fix the login redirect");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let prompt = "Fix the login redirect".to_owned();
    let claude = slopty_proto::thread::AgentId::CLAUDE_CODE.to_owned();
    assert_eq!(
        thread_starts(&mut fake),
        [(claude.clone(), "~".to_owned(), Some(prompt))],
        "an agent's thread, given the task"
    );
    assert!(cx.debug_bounds("ask").is_none(), "its tile is there, starting: no question now");
    // The start's tile closed, the workspace is empty again and asks again.
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        thread_starts(&mut fake),
        [(claude, "~".to_owned(), None)],
        "the field is clear again, and on nothing an agent starts bare"
    );
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();

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

/// The question's chips choose where the agent starts: the directory chip steps through the
/// places shells stand in on the machine, then its home; the machine chip, with several
/// machines, the next machine.
#[gpui::test]
fn the_questions_chips_choose_where_the_agent_starts(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let mut laptop = connect(&view, cx, 2, "laptop");
    let shell = SessionId::new();
    let _tile = opens(&view, cx, &studio, shell, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.session_opened(key, summary(shell, Some("/Users/me/oss/slopty")), cx);
        let last = v.layout.workspaces().len().saturating_sub(1);
        v.go_to_workspace(last, cx);
    });
    cx.run_until_parked();
    let target = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.ask_target());
    let started = |cx: &mut VisualTestContext, fake: &mut Fake| {
        click(cx, "ask-field");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        // The start's tile goes again, so the workspace asks again.
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        thread_starts(fake).into_iter().next().map(|(_, cwd, _)| cwd)
    };
    if target(cx).is_some_and(|(on, _)| on != key) {
        click(cx, "ask-worker");
    }
    let slopty = Some("/Users/me/oss/slopty".to_owned());
    assert_eq!(target(cx), Some((key, slopty)), "the latest place first");
    click(cx, "ask-place");
    assert_eq!(target(cx), Some((key, None)), "then the worker's own default");
    assert_eq!(started(cx, &mut studio).as_deref(), Some("~"), "its home");
    click(cx, "ask-worker");
    assert_eq!(target(cx), Some((laptop.key, None)), "the next worker");
    assert_eq!(started(cx, &mut laptop).as_deref(), Some("~"), "on the laptop, at its home");
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

/// The overview's names are chrome: the same size however far the overview zooms out to fit
/// the workspaces, and so are the words on the place for a new workspace.
#[gpui::test]
fn the_overview_labels_keep_their_size_at_any_zoom(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    three_shells(&view, cx, &fake);
    let overview = |cx: &mut VisualTestContext| {
        cx.simulate_keystrokes("cmd-alt-o");
        cx.run_until_parked();
        let zoom = view.read_with(cx, |v, _| v.drawn.zoom.get());
        let name = cx.debug_bounds("overview-name-0").expect("the first workspace's name");
        let new = cx.debug_bounds("overview-new-workspace-words").expect("the new one's words");
        cx.simulate_keystrokes("cmd-alt-o");
        cx.run_until_parked();
        (zoom, f32::from(name.size.height), f32::from(new.size.height))
    };
    let (zoom, name, new) = overview(cx);
    // The focused tile goes down into a workspace of its own: three to fit, not two.
    cx.simulate_keystrokes("cmd-alt-shift-down");
    cx.run_until_parked();
    let (fitted, name_after, new_after) = overview(cx);
    assert!(fitted < zoom, "more workspaces, smaller: {zoom} then {fitted}");
    assert!(name > 0.0 && new > 0.0);
    assert!((name - name_after).abs() < 0.01, "{name} then {name_after}");
    assert!((new - new_after).abs() < 0.01, "{new} then {new_after}");
}

/// On a desktop the palette hangs a fifth of the way down the window (the one modal anchor)
/// and takes at most three fifths of its height under its ceiling, shorter still when it lists
/// less. On a phone it is a sheet from the top, the window's width,
/// down to the keyboard, with its foot in view. On both a fade covers the list's end while
/// more runs on below it.
#[gpui::test]
fn the_palette_hangs_at_a_fifth_and_is_a_sheet_on_a_phone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    three_shells(&view, cx, &fake);
    // Where the sheet lands, not its rise into place.
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    open_palette(cx);
    let (w, h) = VIEWPORT;
    let brief = cx.debug_bounds("palette").expect("drawn");
    let fifth = h * crate::kit::MODAL_ANCHOR;
    assert!((f32::from(brief.top()) - fifth).abs() < 0.5, "a fifth down: {brief:?}");
    cx.simulate_input("e");
    cx.run_until_parked();
    let full = cx.debug_bounds("palette").expect("drawn");
    let ceiling = crate::kit::Overlay::List.bounds().1.min(h * 0.6);
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
    let foot = cx.debug_bounds("palette-legend").expect("the foot is drawn");
    assert!(f32::from(palette.top()).abs() < 0.5, "from the top: {palette:?}");
    assert!((f32::from(palette.bottom()) - short).abs() < 0.5, "to the keyboard: {palette:?}");
    assert!(
        f32::from(palette.left()).abs() < 0.5 && (f32::from(palette.right()) - phone_w).abs() < 0.5,
        "the window's width: {palette:?}"
    );
    assert!(
        foot.bottom() <= palette.bottom() && foot.top() >= palette.top(),
        "the foot is inside: {foot:?} in {palette:?}"
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
    cx.update(|window, _| crate::retained::faded_edges(window, rows)).bottom
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

/// An agent's line is found by words deep in its first prompt, past what its title shows, and
/// by the answer of its last turn, once its face has read the transcript; a word in neither
/// finds nothing.
#[gpui::test]
fn the_palette_finds_an_agent_by_its_first_prompt_and_last_answer(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| {
        for event in crate::conversation::fixtures::events("edit") {
            v.conversation_event(session, event, cx);
        }
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
    let title = view.read_with(cx, |v, _| v.terminal_title(session));
    assert!(!title.contains("gamma"), "the title stops short of it: {title}");
    assert!(found(cx, "gamma delta"), "by the first prompt");
    assert!(found(cx, "correct step"), "by the last answer");
    assert!(!found(cx, "login redirect"), "by neither");
}

/// The navigator's grouping is a command, and a screen reader hears it as one: an option named
/// for what it does next, which turns to the other once chosen.
#[gpui::test]
fn the_lens_is_an_option_named_for_what_it_does(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _studio = connect(&view, cx, 1, "studio");
    let option = |cx: &mut VisualTestContext, named: &str| {
        open_palette(cx);
        cx.simulate_keystrokes("g r o u p");
        let nodes = tree(cx);
        let found = nodes.iter().any(|n| {
            n.role == "ListBoxOption" && n.label.as_deref().is_some_and(|l| l.starts_with(named))
        });
        assert!(found, "{named}: {nodes:#?}");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    };
    option(cx, navigator::BY_MACHINE);
    let lens = view.read_with(cx, |v, _| v.layout.navigator().group_by.clone());
    assert_eq!(lens, ["machine"], "chosen, it turns");
    option(cx, navigator::BY_PROJECT);
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
    let always = ["New terminal", "New note", "About Slopty", "Overview", "Find in every tile"];
    let of_a_tile = ["Close tile", "Name this tile", "Maximize column", "Move column left"];
    let elsewhere = [
        "Page back",
        "Reload page",
        "Save file",
        SAVE_A_COPY,
        "Mute sound",
        "Stop the agent",
        "Show conversation or terminal",
        "Show project board or terminal",
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
    for label in ["Clear the screen and history", "Find in terminal, file or conversation"] {
        assert!(has(&shell, label), "the terminal's own {label}: {shell:?}");
    }
    for label in elsewhere {
        assert!(!has(&shell, label), "{label} is not a shell's: {shell:?}");
    }

    cx.simulate_keystrokes("cmd-shift-p");
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
        sleeping: false,
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

/// "Scope to atlas" narrows the navigator, the attention sections, the inbox and the status
/// bar's counts alike to atlas; it leads the filter as a token, and Esc lets go of it.
#[gpui::test]
fn a_scope_filters_the_navigator_the_inbox_and_the_counts_alike(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (atlas_agent, atlas) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    let (site_agent, site) = shell_in(&view, cx, &studio, 2, "/w/site", true);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(blocked(atlas_agent), cx);
        v.agent_event(blocked(site_agent), cx);
    });
    cx.run_until_parked();
    let counts = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.agent_counts().iter().map(|(_, c)| c.total()).sum::<usize>())
    };
    assert_eq!(counts(cx), 2);
    let key = view.read_with(cx, |v, _| v.project_groups().group_of(atlas).map(|g| g.key.clone()));
    let scope = ScopeTo { project: key };
    view.update_in(cx, |v, w, cx| v.scope_to(&scope, w, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("nav-scope").is_some(), "the scope leads the filter");
    let listed = view.read_with(cx, |v, _| v.navigator_tiles());
    assert_eq!(listed, [atlas], "only atlas's rows");
    assert_eq!(counts(cx), 1, "the status bar counts atlas's agent alone");
    cx.simulate_keystrokes("cmd-shift-u");
    cx.run_until_parked();
    let row = |session: SessionId| -> &'static str {
        Box::leak(format!("inbox-waiting-{session}").into_boxed_str())
    };
    assert!(cx.debug_bounds(row(atlas_agent)).is_some(), "atlas's agent in the inbox");
    assert!(cx.debug_bounds(row(site_agent)).is_none(), "and not site's");
    cx.simulate_keystrokes("cmd-shift-u");
    cx.run_until_parked();

    click(cx, "nav-filter");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("nav-scope").is_none(), "Esc lets go of it");
    let listed = view.read_with(cx, |v, _| v.navigator_tiles());
    assert!(listed.contains(&site), "every row is back: {listed:?}");
    assert_eq!(counts(cx), 2);
}

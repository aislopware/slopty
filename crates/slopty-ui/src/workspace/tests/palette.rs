//! The palette's sections, the picker's waiting row, the empty workspace and the overview's
//! labels, in the headless workspace.

use gpui::Modifiers;
use slopty_core::{DisplayId, WindowId};
use slopty_proto::screen::{DisplayInfo, WindowInfo};

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
    assert!(tree.iter().any(|n| n.is("Heading", Some("Workers"))), "{tree:#?}");
    assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("New terminal ⌘T"))), "{tree:#?}");

    // "studio" names the worker and where its tile runs, and no command.
    cx.simulate_keystrokes("s t u d i o");
    cx.run_until_parked();
    assert!(top(cx, "palette-heading-tiles").is_some());
    assert!(top(cx, "palette-heading-workers").is_some());
    assert!(top(cx, "palette-heading-commands").is_none(), "an empty group hides its heading");

    // Only the other worker is left: one group, no heading at all.
    cx.simulate_keystrokes("backspace backspace backspace backspace backspace backspace l a p");
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
    view.update(cx, |v, _| v.set_hardware_keyboard(false));
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
    for label in ["New terminal", "New agent", "Add a window or display", "studio"] {
        assert!(tree.iter().any(|n| n.is("Button", Some(label))), "{label}: {tree:#?}");
    }
    assert_eq!(*strip::BEGIN_KEYS, ["⌘T".to_owned(), "⇧⌘T".to_owned(), "⌘O".to_owned()]);

    click(cx, "empty-terminal");
    assert!(
        matches!(
            fake.drain().as_slice(),
            [ClientMsg::OpenSession { spec: OpenSession { command, .. }, .. }] if command.is_empty()
        ),
        "a shell"
    );
    click(cx, "empty-agent");
    assert!(
        matches!(
            fake.drain().as_slice(),
            [ClientMsg::OpenSession { spec: OpenSession { command, .. }, .. }]
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
        let zoom = view.read_with(cx, |v, _| v.drawn_zoom);
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
    assert!(cx.debug_bounds("palette-more").is_some(), "a cut row fades on a desktop too");
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
    assert!(cx.debug_bounds("palette-more").is_some(), "more below: the fade says so");
    // ↑ from the first line wraps to the last, and the list scrolls to its end.
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    next_frame(cx);
    assert!(cx.debug_bounds("palette-more").is_none(), "nothing more below");
}

/// ↓ past the lines in view scrolls the palette's list with the selection, and a query that
/// moves the selection back to the first line brings that line back into view.
#[gpui::test]
fn the_palette_scrolls_to_the_selected_line(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _studio = connect(&view, cx, 1, "studio");
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
    let title = view.read_with(cx, |v, cx| v.terminal_title(session, cx));
    assert!(!title.contains("gamma"), "the title stops short of it: {title}");
    assert!(found(cx, "gamma delta"), "by the first prompt");
    assert!(found(cx, "correct step"), "by the last answer");
    assert!(!found(cx, "login redirect"), "by neither");
}

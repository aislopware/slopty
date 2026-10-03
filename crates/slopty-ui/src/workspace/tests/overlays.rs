//! The overlays in the headless workspace: the inbox's right edge and its empty states, and
//! what the palette reaches and clears out of its way.

use gpui::Modifiers;
use slopty_core::WallMs;

use super::*;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"))
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = bounds(cx, selector).center();
    cx.simulate_click(at, Modifiers::default());
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

/// An inbox row is two lines on the navigator's rhythm. A waiting agent's first line is what it
/// asks, and it ends in how long ago, counted from the worker's stamp so a reconnect keeps it. No
/// word repeats the section heading over it ("Needs you" under *Needs you*), and an agent the
/// hook named only its tool for says what that tool does, not the tool; a failed command's
/// exit leads the second line. The popover hangs flush from the title bar, so no tile header
/// shows through a gap over it, its right edge on the window's inset.
#[gpui::test]
fn an_inbox_row_says_its_age_first_and_no_word_its_heading_does(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (asking, failed, bare) = (SessionId::new(), SessionId::new(), SessionId::new());
    let _asking = opens(&view, cx, &studio, asking, studio.me, 1);
    let _failed = opens(&view, cx, &studio, failed, studio.me, 2);
    let _bare = opens(&view, cx, &studio, bare, studio.me, 3);
    let _here = opens(&view, cx, &studio, SessionId::new(), studio.me, 4);
    let since_ms = inbox::wall_ms().saturating_sub(5 * 60_000);
    view.update_in(cx, |v, _w, cx| {
        let status = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() });
        let detail = Some("$ touch refused.txt".into());
        v.agent_event(
            AgentEvent {
                since_ms: WallMs::from_millis(since_ms),
                status: status.clone(),
                detail,
                ..blocked(asking)
            },
            cx,
        );
        v.agent_event(
            AgentEvent {
                since_ms: WallMs::from_millis(since_ms),
                status,
                detail: None,
                ..blocked(bare)
            },
            cx,
        );
        let done = Finished {
            command: "cargo clippy".into(),
            exit: Some(101),
            elapsed: Duration::from_secs(40),
        };
        v.command_finished(failed, done, cx);
    });
    cx.run_until_parked();
    // Where the popover rests, not its drop into place.
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    click(cx, "bell");

    let theme = Theme::default();
    let inbox = bounds(cx, "inbox");
    let top = f32::from(inbox.top());
    assert!(top >= TITLEBAR_H - 0.5 && top < TITLEBAR_H + theme.spacing.xs - 0.5, "{inbox:?}");
    let right = VIEWPORT.0 - theme.spacing.inset();
    assert!((f32::from(inbox.right()) - right).abs() < 0.5, "on the inset: {inbox:?}");
    let two = crate::kit::Row::Two.height(&theme);
    for row in [format!("inbox-waiting-{asking}"), format!("inbox-finished-{failed}")] {
        let (edge, ago) = (bounds(cx, leak(row.clone())), bounds(cx, leak(format!("{row}-age"))));
        assert!((f32::from(edge.size.height) - two).abs() < 0.5, "the navigator's rhythm");
        assert!(ago.center().y < edge.center().y, "line one: {ago:?} in {edge:?}");
        let inset = f32::from(edge.right()) - f32::from(ago.right());
        assert!(inset < theme.spacing.inset() + 0.5, "at the right edge: {ago:?} {edge:?}");
    }
    let waiting = format!("inbox-waiting-{asking}-word");
    assert!(cx.debug_bounds(leak(waiting)).is_none(), "no \"Needs you\" under Needs you");
    let exit = bounds(cx, leak(format!("inbox-finished-{failed}-word")));
    let age = bounds(cx, leak(format!("inbox-finished-{failed}-age")));
    assert!(exit.top() >= age.bottom() - px(0.5), "the exit on the second line: {exit:?}");
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l.contains("Exit 101")));
    assert!(
        names.iter().any(|l| l.starts_with("Run touch refused.txt")),
        "what it asks, not \"Needs approval\" under Needs you: {names:#?}"
    );
    assert!(
        names.iter().any(|l| l.starts_with("Wants to run a command \u{b7} ")),
        "a bare tool says what it asks, not \"Bash\": {names:#?}"
    );
}

/// The inbox drops the base unit from the bell as it fades in, and under Reduce Motion is in
/// place at once. The fill under the view that is up is the one plate, and choosing *All* puts
/// it there.
#[gpui::test]
fn the_inbox_drops_in_and_its_view_sits_on_the_plate(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _studio = connect(&view, cx, 1, "studio");
    click(cx, "bell");
    let arriving = bounds(cx, "inbox");
    click(cx, "bell");
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    click(cx, "bell");
    let rest = bounds(cx, "inbox");
    let drop = f32::from(rest.top() - arriving.top());
    let unit = Theme::default().spacing.xs;
    assert!(drop > unit - 1.0 && drop < unit + 0.01, "from a base unit above: {drop}");
    let plate = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.inbox_plate());
    assert_eq!(plate(cx), Some(bounds(cx, "inbox-unread")), "under the view that is up");
    click(cx, "inbox-all");
    assert_eq!(plate(cx), Some(bounds(cx, "inbox-all")), "gone to All");
}

/// The inbox's empty states come in two tiers: one that has never held anything says what
/// lands in it; one emptied by reading is a quiet line.
#[gpui::test]
fn an_empty_inbox_explains_itself_once(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let built = SessionId::new();
    let _built = opens(&view, cx, &studio, built, studio.me, 1);
    let _here = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    click(cx, "bell");
    assert!(cx.debug_bounds("inbox-empty").is_some());
    assert!(cx.debug_bounds("inbox-holds").is_some(), "a new inbox says what it is for");
    let fresh = bounds(cx, "inbox-empty");

    click(cx, "bell");
    view.update_in(cx, |v, _w, cx| {
        let done =
            Finished { command: "make".into(), exit: Some(0), elapsed: Duration::from_secs(40) };
        v.command_finished(built, done, cx);
    });
    cx.run_until_parked();
    click(cx, "bell");
    click(cx, "inbox-mark-all");
    let read = bounds(cx, "inbox-empty");
    assert!(cx.debug_bounds("inbox-holds").is_none(), "read empty: no explanation");
    assert!(read.size.height < fresh.size.height, "one quiet line: {read:?} {fresh:?}");
}

/// The Tiles section lists every tile the navigator does, an unnamed note included, by its
/// title. Its context says how far the note got in the words the header's count and the
/// navigator's second line use: one way of saying it, not three.
#[gpui::test]
fn the_palette_finds_an_unnamed_note(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let kind = ItemKind::Note { text: "Release\n- [x] tag\n- [ ] notes\n".into() };
    let note = arrives(&view, cx, &studio, kind, 2);
    let lines = view.update(cx, |v, cx| v.palette_lines(cx));
    let line = lines
        .iter()
        .find(|l| matches!(l.run, PaletteRun::Item(id) if id == note.item))
        .unwrap_or_else(|| panic!("the note is listed: {lines:#?}"));
    assert_eq!(line.section, crate::palette::Section::Tiles);
    assert!(line.label.starts_with("Release"), "{line:?}");
    assert_eq!(line.place.as_deref(), Some("1 of 2 done"), "{line:?}");
    assert_eq!(line.trailing(), None, "the place is not the right edge's");
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Heading", Some("note Release"))), "named: {tree:#?}");
    assert!(tree.iter().any(|n| n.is("Status", Some("1 of 2 done"))), "counted: {tree:#?}");
    let rows = view.read_with(cx, WorkspaceView::navigator_lines);
    assert!(rows.iter().any(|(_, meta, _)| meta == "1 of 2 done"), "the navigator: {rows:#?}");
}

/// On a phone the drawer and the palette never stack: opening the palette puts the drawer
/// away, since whatever the palette goes to is under it.
#[gpui::test]
fn opening_the_palette_puts_the_phone_drawer_away(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_resize(size(px(390.0), px(760.0)));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_some(), "the drawer is out");
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_some());
    assert!(cx.debug_bounds("navigator").is_none(), "and put away for the palette");
}

/// A dismissed palette is closed at once, since the keys go back where they were, but it is
/// drawn for its way out and only then dropped; under Reduce Motion it goes in the same frame.
#[gpui::test]
fn a_dismissed_palette_draws_its_way_out(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.palette_open()), "closed to the keys");
    assert!(cx.debug_bounds("palette").is_some(), "still drawing its way out");
    cx.executor().advance_clock(crate::kit::Pace::Fade.duration());
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_none(), "dropped once out");

    cx.update(|_w, cx| cx.set_reduce_motion(true));
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_none(), "gone in the same frame");
}

/// What floats as a sheet nests its rows: a menu's, the palette's and the inbox's rows sit the
/// sheet's hairline and pad in from its edge, so a row's 6 pt corner shares the 12 pt sheet's
/// centre.
#[gpui::test]
fn a_sheets_rows_nest_in_its_corners(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let built = SessionId::new();
    let _built = opens(&view, cx, &studio, built, studio.me, 1);
    // Focus elsewhere, so the build finishes unwatched and the inbox lists it.
    let _last = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    let theme = Theme::default();
    // From the sheet's outer edge: its hairline and its pad, which with a row's radius make
    // the sheet's.
    let pad = crate::kit::sheet_pad(&theme) + theme.hair();
    assert!((pad + theme.radii.sm - theme.radii.lg).abs() < f32::EPSILON, "concentric");
    let inset = |cx: &mut VisualTestContext, sheet: &'static str, row: &'static str| {
        let (sheet, row) = (
            cx.debug_bounds(sheet).unwrap_or_else(|| panic!("{sheet}")),
            cx.debug_bounds(row).unwrap_or_else(|| panic!("{row}")),
        );
        f32::from(row.left() - sheet.left())
    };
    let click = |cx: &mut VisualTestContext, selector: &'static str| {
        let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector}"));
        cx.simulate_click(at.center(), gpui::Modifiers::default());
        cx.run_until_parked();
    };

    click(cx, "more");
    let at = inset(cx, "menu", "menu-Command palette");
    assert!((at - pad).abs() < 0.5, "a menu's row: {at}");
    click(cx, "more");

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    let at = inset(cx, "palette", "palette-item-0");
    assert!((at - pad).abs() < 0.5, "the palette's row: {at}");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    view.update_in(cx, |v, _w, cx| {
        let done = Finished {
            command: "cargo build".into(),
            exit: Some(0),
            elapsed: Duration::from_secs(40),
        };
        v.command_finished(built, done, cx);
    });
    cx.run_until_parked();
    click(cx, "bell");
    let row = Box::leak(format!("inbox-finished-{built}").into_boxed_str());
    let at = inset(cx, "inbox", row);
    assert!((at - pad).abs() < 0.5, "the inbox's row: {at}");
}

/// A bar menu closes on Esc, which the shell under it never gets, and on a second press of
/// its button.
#[gpui::test]
fn a_bar_menu_closes_on_escape_and_a_second_press(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    click(cx, "more");
    assert!(cx.debug_bounds("menu").is_some(), "open");
    fake.drain();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("menu").is_none(), "Esc closes it");
    let typed = fake
        .drain()
        .into_iter()
        .any(|m| matches!(m, ClientMsg::Term { req: TermRequest::Key(_), .. }));
    assert!(!typed, "the shell does not get the Esc");
    click(cx, "more");
    click(cx, "more");
    assert!(cx.debug_bounds("menu").is_none(), "a second press closes it");
}

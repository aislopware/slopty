//! The overlays in the headless workspace: the inbox's right edge and its empty states, and
//! what the palette reaches and clears out of its way.

use gpui::Modifiers;

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

/// An inbox row is two lines on the navigator's rhythm. The first ends in how long ago, a
/// waiting agent's too, counted from the worker's stamp so a reconnect keeps it. No word
/// repeats the section heading over it ("Needs you" under *Needs you*); a failed command's
/// exit leads the second line. The popover stands a base unit clear of the title bar, its right
/// edge on the window's inset.
#[gpui::test]
fn an_inbox_row_says_its_age_first_and_no_word_its_heading_does(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (asking, failed) = (SessionId::new(), SessionId::new());
    let _asking = opens(&view, cx, &studio, asking, studio.me, 1);
    let _failed = opens(&view, cx, &studio, failed, studio.me, 2);
    let _here = opens(&view, cx, &studio, SessionId::new(), studio.me, 3);
    let since_ms = inbox::wall_ms().saturating_sub(5 * 60_000);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { since_ms, ..blocked(asking) }, cx);
        let done = Finished {
            command: "cargo clippy".into(),
            exit: Some(101),
            elapsed: Duration::from_secs(40),
        };
        v.command_finished(failed, done, cx);
    });
    cx.run_until_parked();
    click(cx, "bell");

    let theme = Theme::default();
    let inbox = bounds(cx, "inbox");
    assert!(f32::from(inbox.top()) >= TITLEBAR_H + theme.spacing.xs - 0.5, "{inbox:?}");
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
    assert!(labels(&view, cx).iter().any(|l| l.contains("Exit 101")));
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

/// The Tiles section lists every tile the navigator does, an unnamed note included, by the
/// title its header shows, its kind in the muted context and not on the right edge.
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
    assert_eq!(line.kind.as_deref(), Some("Note"), "{line:?}");
    assert_eq!(line.trailing(), None, "the kind is not the right edge's");
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

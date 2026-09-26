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

/// An inbox row reads down its right edge: the status word on its first line, in the word a
/// scan of the fleet reads ("Needs you", "Exit 101"), and how long ago on its second, a waiting
/// agent's too, counted from the worker's stamp so a reconnect keeps it. The popover stands a
/// base unit clear of the title bar.
#[gpui::test]
fn an_inbox_row_says_its_status_and_age_down_the_right_edge(cx: &mut TestAppContext) {
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

    let gap = Theme::default().spacing.xs;
    let inbox = bounds(cx, "inbox");
    assert!(f32::from(inbox.top()) >= TITLEBAR_H + gap - 0.5, "{inbox:?}");
    let names = labels(&view, cx);
    for (row, word) in [
        (format!("inbox-waiting-{asking}"), "Needs you"),
        (format!("inbox-finished-{failed}"), "Exit 101"),
    ] {
        let (edge, said, ago) = (
            bounds(cx, leak(row.clone())),
            bounds(cx, leak(format!("{row}-word"))),
            bounds(cx, leak(format!("{row}-age"))),
        );
        let right = f32::from(edge.right());
        assert!((f32::from(said.right()) - f32::from(ago.right())).abs() < 0.5, "{said:?} {ago:?}");
        assert!(right - f32::from(said.right()) < Theme::default().spacing.inset() + 0.5);
        assert!(ago.top() > said.top(), "the word on the first line, the age under it");
        assert!(names.iter().any(|l| l.contains(word)), "{word}: {names:#?}");
    }
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

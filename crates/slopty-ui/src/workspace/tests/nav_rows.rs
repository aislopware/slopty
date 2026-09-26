//! The navigator's rows in the headless workspace: the filter, a tile's two lines, a folded
//! worker's rollup and the "+" on a worker's header.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gpui::Modifiers;

use super::*;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

fn shown(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

/// Type `text` into the navigator's filter.
fn filter(cx: &mut VisualTestContext, text: &str) {
    click(cx, "nav-filter");
    cx.simulate_input(text);
    cx.run_until_parked();
}

/// The filter keeps the tiles whose title or second line has what was typed, and every tile
/// of a worker whose name has it. A fold hides nothing while it filters; with nothing left it
/// says so; ↩ goes to the first tile listed, and Esc empties it.
#[gpui::test]
fn the_filter_keeps_the_rows_that_match(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let slopty =
        opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/w/oss/slopty"));
    let site = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 2, Some("/w/web/site"));
    let away = opens_in(&view, cx, &laptop, SessionId::new(), laptop.me, 1, Some("/w/notes"));
    let row = |t: TileRef| selector("nav-tile", t.item);
    click(cx, leak(format!("nav-worker-{}", studio.key)));
    assert!(!shown(cx, row(slopty)), "folded");

    filter(cx, "SLOPTY");
    assert_eq!(view.read_with(cx, |v, _| v.navigator_filter().to_owned()), "SLOPTY");
    assert!(shown(cx, row(slopty)), "a match shows through the fold, whatever its case");
    assert!(!shown(cx, row(site)) && !shown(cx, row(away)), "the rest do not");
    assert!(!shown(cx, leak(format!("nav-worker-{}", laptop.key))), "nor a worker with none");

    cx.simulate_keystrokes("backspace backspace backspace backspace backspace backspace");
    filter(cx, "lapt");
    assert!(shown(cx, row(away)), "a worker's name keeps every tile of it");
    assert!(!shown(cx, row(slopty)));

    filter(cx, "zzz");
    assert!(shown(cx, "nav-nothing"), "says so when nothing is left");

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.navigator_filter().to_owned()), "");
    assert!(!shown(cx, row(slopty)), "the fold is back");
    assert!(shown(cx, row(away)));

    filter(cx, "site");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(site), "↩ goes to the first tile listed");
}

/// A shell's row reads two lines: its title, then its directory, what its agent says, its
/// branch and its age from the session's start. The second line sits under the title, and the
/// age ends on the row's right.
#[gpui::test]
fn a_tile_row_reads_its_place_its_words_its_branch_and_its_age(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let started =
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis().saturating_sub(300_000);
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::Terminal { session },
        sleeping: false,
        name: None,
    };
    let tile = TileRef { worker: studio.key, item: item.id };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        let summary = SessionSummary {
            branch: Some("main".into()),
            started_ms: u64::try_from(started).unwrap(),
            ..summary(session, Some("/Users/me/oss/slopty"))
        };
        v.session_opened(key, summary, cx);
        let by = studio.me;
        v.apply_sync(key, ItemSync::Delta { version: 1, by, op: ItemOp::Upsert(item) }, cx);
        v.agent_event(blocked(session), cx);
    });
    cx.run_until_parked();
    let words = agent_status_text(&blocked(session));
    let lines = view.read_with(cx, WorkspaceView::navigator_lines);
    assert_eq!(
        lines,
        [(
            "shell".to_owned(),
            format!("oss/slopty \u{2022} {words} \u{2022} main"),
            Some("5m".into())
        )]
    );
    let id = tile.item.as_uuid();
    let (row, meta, age) = (
        cx.debug_bounds(selector("nav-tile", tile.item)).expect("the row"),
        cx.debug_bounds(leak(format!("nav-meta-{id}"))).expect("the second line"),
        cx.debug_bounds(leak(format!("nav-age-{id}"))).expect("the age"),
    );
    assert!(meta.top() > row.top() + (row.size.height / 3.0), "under the title: {meta:?} {row:?}");
    assert!(age.right() <= row.right(), "{age:?} {row:?}");
    assert!(meta.right() <= age.left(), "the age after the line: {age:?} {meta:?}");
}

/// Folded, a worker's header shows what its tiles add up to in its slot: the warn mark while
/// one waits on the human, the working mark while one works, the unseen dot for a command
/// that finished unwatched, and nothing at rest. Unfolded, the rows say it themselves.
#[gpui::test]
fn a_folded_worker_rolls_up_what_its_tiles_want(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(first, _), (second, _), _] = three_shells(&view, cx, &studio);
    let rollup = leak(format!("nav-rollup-{}", studio.key));
    let header = leak(format!("nav-worker-{}", studio.key));
    click(cx, header);
    let away = point(px(VIEWPORT.0 - 10.0), px(VIEWPORT.1 / 2.0));
    cx.simulate_mouse_move(away, None, Modifiers::default());
    cx.run_until_parked();
    assert!(!shown(cx, rollup), "at rest, nothing");

    view.update_in(cx, |v, _w, cx| {
        let done =
            Finished { command: "make".into(), exit: Some(0), elapsed: Duration::from_secs(9) };
        v.command_finished(first, done, cx);
    });
    cx.run_until_parked();
    // The marks in the header's slot only: the tile headers and the tab carry their own.
    let slot = leak(format!("nav-worker-slot-{}", studio.key));
    let marks = |cx: &mut VisualTestContext| {
        cx.update(|window, _cx| window.set_a11y_active(true));
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let at = cx.debug_bounds(slot).expect("the slot is drawn");
        let inside = |b: [f32; 4]| {
            b[0] >= f32::from(at.left()) - 0.5 && b[0] + b[2] <= f32::from(at.right()) + 0.5
        };
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        tree.into_iter()
            .filter(|n| n.role == "Image" && inside(n.bounds))
            .filter(|n| n.bounds[1] >= f32::from(at.top()) - 0.5)
            .filter(|n| n.bounds[1] + n.bounds[3] <= f32::from(at.bottom()) + 0.5)
            .filter_map(|n| n.label)
            .collect::<Vec<_>>()
    };
    assert!(shown(cx, rollup));
    assert!(marks(cx).iter().any(|m| m == "Unseen"), "{:?}", marks(cx));

    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(second) }, cx);
    });
    cx.run_until_parked();
    assert!(marks(cx).iter().any(|m| m == "Working"), "work outranks news: {:?}", marks(cx));
    assert!(!marks(cx).iter().any(|m| m == "Unseen"));

    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(first), cx));
    cx.run_until_parked();
    assert!(marks(cx).iter().any(|m| m == "Needs you"), "waiting outranks work: {:?}", marks(cx));
    assert!(!marks(cx).iter().any(|m| m == "Working"));

    click(cx, header);
    assert!(!shown(cx, rollup), "unfolded, the rows say it");
}

/// The pointer over a worker's header brings out "+", which opens a shell on that worker and
/// leaves the header's fold alone.
#[gpui::test]
fn the_plus_on_a_worker_opens_a_shell_there(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _studio = connect(&view, cx, 1, "studio");
    let mut laptop = connect(&view, cx, 2, "laptop");
    let tile = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 1);
    let header = cx.debug_bounds(leak(format!("nav-worker-{}", laptop.key))).expect("drawn");
    cx.simulate_mouse_move(header.center(), None, Modifiers::default());
    cx.run_until_parked();
    click(cx, leak(format!("nav-new-shell-{}", laptop.key)));
    let sent = laptop.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::OpenSession(o) if o.command.is_empty())),
        "{sent:?}"
    );
    assert!(shown(cx, selector("nav-tile", tile.item)), "the header did not fold");
}

/// What the navigator adds to a frame over many tiles: the same workspace drawn with it
/// docked and hidden, in one build. Run by hand (it prints, it does not judge);
/// `docs/MEASUREMENTS.md` has the numbers and the command.
#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_the_navigator_over_many_tiles(cx: &mut TestAppContext) {
    const SHELLS: usize = 60;
    const NOTES: usize = 60;
    const FRAMES: usize = 400;
    const WARM: usize = 20;
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let mut items: Vec<Item> = Vec::new();
    let mut sessions = Vec::new();
    for n in 0..SHELLS {
        let session = SessionId::new();
        let started = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
        sessions.push(SessionSummary {
            branch: Some("main".into()),
            started_ms: u64::try_from(started).unwrap(),
            ..summary(session, Some(&format!("/Users/me/src/project_{n}")))
        });
        items.push(Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            sleeping: false,
            name: None,
        });
    }
    for _ in 0..NOTES {
        let kind = ItemKind::Note { text: "a note\n".into() };
        items.push(Item { id: ItemId::new(), kind, sleeping: false, name: None });
    }
    let key = studio.key;
    view.update_in(cx, |v, _window, cx| {
        for s in sessions {
            v.session_opened(key, s, cx);
        }
        v.apply_sync(key, ItemSync::Snapshot { version: 1, items }, cx);
    });
    cx.run_until_parked();
    let time = |cx: &mut VisualTestContext| {
        let mut took = Vec::with_capacity(FRAMES);
        for n in 0..WARM + FRAMES {
            let start = Instant::now();
            view.update(cx, |_, cx| cx.notify());
            cx.run_until_parked();
            if n >= WARM {
                took.push(start.elapsed());
            }
        }
        took.sort_unstable();
        let pct = |p: usize| slopty_client::pacing::percentile(&took, p).as_secs_f64() * 1e3;
        (pct(50), pct(95))
    };
    assert!(cx.debug_bounds("navigator").is_some());
    let docked = time(cx);
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_none());
    let hidden = time(cx);
    println!(
        "MEASURE navigator, {SHELLS} shells + {NOTES} notes, {FRAMES} frames: docked p50 {:.3} \
         ms p95 {:.3} ms; hidden p50 {:.3} ms p95 {:.3} ms",
        docked.0, docked.1, hidden.0, hidden.1
    );
}

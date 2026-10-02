//! The navigator's rows in the headless workspace: the filter, a tile's two lines, a folded
//! worker's rollup and the "+" on a worker's header.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gpui::Modifiers;
use slopty_client::layout::NavLens;
use slopty_core::WallMs;

use super::super::actions::ToggleNavigatorLens;
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

/// ⌘⇧E shows a hidden navigator and puts the keyboard in its filter, so what is typed next
/// narrows the rows.
#[gpui::test]
fn a_key_types_into_the_navigators_filter(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let slopty =
        opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/w/oss/slopty"));
    let site = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 2, Some("/w/web/site"));
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(!shown(cx, "nav-filter"), "hidden");
    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    assert!(shown(cx, "nav-filter"), "shown again");
    cx.simulate_input("site");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.navigator_filter().to_owned()), "site");
    let row = |t: TileRef| selector("nav-tile", t.item);
    assert!(shown(cx, row(site)) && !shown(cx, row(slopty)), "typed, it narrows");
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

/// A shell no client views still shows its program's progress from its summary: the figure at
/// the end of its second line and the hairline under it, along the share it names; a reopened
/// shell says "Restored" there; and both go when the summary drops them.
#[gpui::test]
fn a_row_shows_its_sessions_progress_and_that_it_was_restored(cx: &mut TestAppContext) {
    use slopty_proto::terminal::{Progress, ProgressState, Restored};
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens_in(&view, cx, &studio, session, studio.me, 1, Some("/w/oss/slopty"));
    let id = tile.item.as_uuid();
    let key = studio.key;
    let reported = SessionSummary {
        progress: Some(Progress { state: ProgressState::Set, percent: Some(40) }),
        restored: Some(Restored { saved_ms: WallMs::ZERO, command: Vec::new() }),
        ..summary(session, Some("/w/oss/slopty"))
    };
    view.update_in(cx, |v, _w, cx| v.session_opened(key, reported, cx));
    cx.run_until_parked();
    let row = cx.debug_bounds(selector("nav-tile", tile.item)).expect("the row");
    let figure = cx.debug_bounds(leak(format!("nav-progress-{id}"))).expect("the figure");
    let bar = cx.debug_bounds(leak(format!("nav-progress-bar-{id}"))).expect("the hairline");
    let meta = cx.debug_bounds(leak(format!("nav-meta-{id}"))).expect("the second line");
    assert!(shown(cx, leak(format!("nav-restored-{id}"))), "the restored mark");
    assert!(figure.top() >= meta.top() - px(0.5), "on the second line: {figure:?} {meta:?}");
    assert!(bar.top() >= meta.bottom() - px(0.5) && bar.bottom() <= row.bottom(), "{bar:?}");
    let lines = cx.debug_bounds(leak(format!("nav-lines-{id}"))).expect("the lines");
    let share = f32::from(bar.size.width) / f32::from(lines.right() - bar.left());
    assert!(share > 0.3 && share < 0.5, "about 40% of the lines: {share}");

    let quiet = summary(session, Some("/w/oss/slopty"));
    view.update_in(cx, |v, _w, cx| v.session_opened(key, quiet, cx));
    cx.run_until_parked();
    assert!(!shown(cx, leak(format!("nav-progress-{id}"))), "the report ended");
    assert!(!shown(cx, leak(format!("nav-progress-bar-{id}"))));
    assert!(!shown(cx, leak(format!("nav-restored-{id}"))));
}

/// A shell's row reads two lines: its title, ended by its age from the session's start, then
/// what its agent says, its directory and its branch, in the order every second line keeps. Once
/// the agent waits on the human, the state takes the age's place in a word and the second line
/// says what it asks, not the state again; the row is not washed: the word is the one mark it
/// needs. Folded away, *Needs you* lists it.
#[gpui::test]
fn a_tile_row_reads_its_age_or_its_state_then_its_place(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let other = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
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
            changes: None,
            started_ms: WallMs::from_millis(u64::try_from(started).unwrap()),
            ..summary(session, Some("/Users/me/oss/slopty"))
        };
        v.session_opened(key, summary, cx);
        let by = studio.me;
        v.apply_sync(key, ItemSync::Delta { version: 2, by, op: ItemOp::Add(item) }, cx);
    });
    cx.run_until_parked();
    let id = tile.item.as_uuid();
    let (row, meta, age) = (
        cx.debug_bounds(selector("nav-tile", tile.item)).expect("the row"),
        cx.debug_bounds(leak(format!("nav-meta-{id}"))).expect("the second line"),
        cx.debug_bounds(leak(format!("nav-age-{id}"))).expect("the age"),
    );
    assert!(meta.top() > row.top() + (row.size.height / 3.0), "under the title: {meta:?} {row:?}");
    assert!(age.bottom() <= meta.top(), "the age ends the first line: {age:?} {meta:?}");
    assert!(age.right() <= row.right() && age.left() > row.center().x, "{age:?} {row:?}");

    // Focus the other shell, so the waiting one's row is not the selected one.
    click(cx, selector("nav-tile", other.item));
    // And take the pointer away: the waiting row moves up under it, where its end would give
    // way to the close button.
    cx.simulate_mouse_move(point(px(1.0), px(1.0)), None, Modifiers::default());
    let asks = AgentEvent {
        status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
        detail: Some("$ touch refused.txt".into()),
        ..blocked(session)
    };
    view.update_in(cx, |v, _w, cx| v.agent_event(asks, cx));
    cx.run_until_parked();
    let lines = view.read_with(cx, WorkspaceView::navigator_lines);
    assert!(
        lines.contains(&(
            "Claude Code".to_owned(),
            "Run touch refused.txt \u{b7} oss/slopty \u{b7} main".to_owned(),
            Some("5m".into())
        )),
        "what it asks, not its state again: {lines:#?}"
    );
    // The state is the glyph that leads the row, not a word that takes the title's room: the
    // line's end keeps the age, and the row's name still says the state.
    let age = cx.debug_bounds(leak(format!("nav-age-{id}"))).expect("the age stays");
    let meta = cx.debug_bounds(leak(format!("nav-meta-{id}"))).expect("the second line");
    assert!(age.bottom() <= meta.top() + px(0.5), "on the first line: {age:?} {meta:?}");
    assert!(cx.debug_bounds(leak(format!("nav-status-{id}"))).is_none(), "no state word");
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Image", Some("Needs you"))), "the glyph: {nodes:#?}");
    assert!(
        nodes.iter().any(|n| n.label.as_deref().is_some_and(|l| l.contains("Needs approval"))),
        "the row's name says it"
    );
    let wash = gpui::Background::from(crate::colors::hsla_alpha(
        Theme::default().surfaces.warn_fill,
        slopty_theme::alpha::FAINT,
    ));
    let row = cx.debug_bounds(selector("nav-tile", tile.item)).expect("the row");
    let washed = cx.update(|window, _| {
        let scale = window.scale_factor();
        let near = |a: f32, b: Pixels| f32::from(b).mul_add(-scale, a).abs() < 1.0;
        window.painted_quads().into_iter().any(|q| {
            q.background == wash
                && near(q.bounds.origin.y.0, row.origin.y)
                && near(q.bounds.size.height.0, row.size.height)
        })
    });
    assert!(!washed, "a row waiting on the human is not washed a second time");

    // Folded out of sight, it is listed under *Needs you*, whose row joins the agent's words and
    // its place as a tile's second line does: the one separator, spaces and all, flush against
    // both.
    assert!(!shown(cx, "nav-needs-you"), "in view, its own row says it");
    click(cx, leak(format!("nav-worker-{key}")));
    assert!(shown(cx, "nav-needs-you"), "folded away, the section lists it");
    let mut part = |name: &str| {
        cx.debug_bounds(leak(format!("nav-waiting-{name}-{session}")))
            .unwrap_or_else(|| panic!("the agent row's {name}"))
    };
    let (words, separator, place) = (part("words"), part("separator"), part("place"));
    assert!((separator.left() - words.right()).abs() < px(0.5), "{words:?} {separator:?}");
    assert!((place.left() - separator.right()).abs() < px(0.5), "{separator:?} {place:?}");
    let spaced = cx.update(|window, _cx| {
        let font_size = px(Theme::default().typography.meta());
        let run = |len| gpui::TextRun {
            len,
            font: window.text_style().font(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let text = rollup::META_SEPARATOR;
        let dot = "\u{b7}";
        let shape = |s: &'static str| {
            window.text_system().shape_line(s.into(), font_size, &[run(s.len())], None).width
        };
        (shape(text), shape(dot))
    });
    assert!(separator.size.width > spaced.1 + px(1.0), "the spaces stay: {spaced:?}");
}

/// An open worker with no tile says so in one quiet line on its tiles' edge; a filter that
/// matched its name lists it bare, and its first tile takes the line's place.
#[gpui::test]
fn a_worker_with_no_tile_says_so_quietly(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let vacant = leak(format!("nav-vacant-{}", laptop.key));
    assert!(shown(cx, vacant), "an empty worker says so");
    assert!(!shown(cx, leak(format!("nav-vacant-{}", studio.key))), "one with a tile does not");
    filter(cx, "lapt");
    assert!(shown(cx, leak(format!("nav-worker-{}", laptop.key))) && !shown(cx, vacant));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let tile = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 1);
    assert!(shown(cx, selector("nav-tile", tile.item)) && !shown(cx, vacant), "a tile takes it");
}

/// A note's row says how far its tasks got, not that it is a note.
#[gpui::test]
fn a_note_row_says_its_progress(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let text = "# Release\n- [x] tag\n- [ ] notes\n- [ ] ship\n".to_owned();
    let _note = arrives(&view, cx, &studio, ItemKind::Note { text }, 1);
    let lines = view.read_with(cx, WorkspaceView::navigator_lines);
    assert!(lines.iter().any(|(_, meta, _)| meta == "1 of 3 done"), "{lines:#?}");
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
        sent.iter()
            .any(|m| matches!(m, ClientMsg::OpenSession { spec: o, .. } if o.command.is_empty())),
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
            changes: None,
            started_ms: WallMs::from_millis(u64::try_from(started).unwrap()),
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

/// A tile's two lines sit in the middle of its row at either density: at the touch height the
/// row grows round them, not under them.
#[gpui::test]
fn a_rows_two_lines_sit_in_its_middle(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let tile = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/w/oss/app"));
    let id = tile.item.as_uuid();
    for (density, height) in
        [(slopty_theme::Density::COMPACT, 40.0), (slopty_theme::Density::TOUCH, 56.0)]
    {
        let theme = Theme { density, ..Theme::default() };
        view.update(cx, |v, cx| v.set_theme(theme, cx));
        cx.run_until_parked();
        let row = cx.debug_bounds(selector("nav-tile", tile.item)).expect("the row");
        let lines = cx.debug_bounds(leak(format!("nav-lines-{id}"))).expect("the lines");
        assert!((f32::from(row.size.height) - height).abs() < 0.5, "{density:?}: {row:?}");
        let (above, below) = (lines.top() - row.top(), row.bottom() - lines.bottom());
        // Within the pixel the layout rounds to.
        assert!(
            f32::from(above - below).abs() <= 1.0,
            "{density:?}: {above:?} over, {below:?} under"
        );
    }
}

/// The focused tile's row sits on the navigator's plate, with no fill of its own; a click on
/// another row puts the plate there (at once, under Reduce Motion).
#[gpui::test]
fn the_selected_row_sits_on_the_plate(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    let plate = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.navigator_plate());
    for tile in [first, second, first] {
        click(cx, selector("nav-tile", tile.item));
        assert_eq!(focused(&view, cx), Some(tile));
        let row = cx.debug_bounds(selector("nav-tile", tile.item)).expect("the row");
        assert_eq!(plate(cx), Some(row), "under the focused row");
    }
}

/// The plate glides on the workspace's clock, the one the springs run on: held still, it
/// stays where the clock puts it however long the frames take to draw, part of the way there
/// at a part of the glide and on the row once the glide is over. On the wall clock, a frame
/// drawn again later drew it further on than the frame on screen.
#[gpui::test]
fn the_plate_glides_on_the_workspaces_clock(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    view.update(cx, |v, _| {
        v.set_animation(true);
        v.hold_clock(Some(Duration::ZERO));
    });
    let plate = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.navigator_plate());
    let row = |cx: &mut VisualTestContext, tile: TileRef| {
        cx.debug_bounds(selector("nav-tile", tile.item)).expect("the row")
    };
    let settled = Duration::from_secs(5);
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    view.update(cx, |v, _| v.hold_clock(Some(settled)));
    cx.update(Window::simulate_next_frame);
    cx.run_until_parked();
    let from = row(cx, first);
    assert_eq!(plate(cx), Some(from), "on the first row");

    view.update_in(cx, |v, _w, cx| v.focus_tile(second, cx));
    cx.run_until_parked();
    let to = row(cx, second);
    assert_eq!(plate(cx), Some(from), "the glide starts where the plate was");
    let half = settled.saturating_add(crate::kit::Pace::Settle.duration().div_f32(2.0));
    view.update(cx, |v, _| v.hold_clock(Some(half)));
    cx.update(Window::simulate_next_frame);
    cx.run_until_parked();
    let midway = plate(cx).expect("a plate").top();
    assert!(from.top() < midway && midway < to.top(), "{from:?} → {midway:?} → {to:?}");
    cx.update(Window::simulate_next_frame);
    cx.run_until_parked();
    assert_eq!(plate(cx).map(|p| p.top()), Some(midway), "the clock held, the plate holds");

    view.update(cx, |v, _| v.hold_clock(Some(settled.saturating_mul(2))));
    cx.update(Window::simulate_next_frame);
    cx.run_until_parked();
    assert_eq!(plate(cx), Some(to), "landed on the second row");
}

/// One agent is one row. At work with its tile's row in view, *Working* does not list it again:
/// the row ends in "Working" already. Folded away, *Working* lists it. Waiting, its row ends in
/// the state's own word ("Needs approval"), not the section's "Needs you".
#[gpui::test]
fn an_agent_in_view_is_listed_once_in_its_own_word(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (busy, asking) = (SessionId::new(), SessionId::new());
    let _busy = opens(&view, cx, &studio, busy, studio.me, 1);
    let _asking = opens(&view, cx, &studio, asking, studio.me, 2);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(busy) }, cx);
        let status = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() });
        v.agent_event(AgentEvent { status, ..blocked(asking) }, cx);
    });
    cx.run_until_parked();
    assert!(!shown(cx, "nav-working"), "its row says it works");
    assert!(view.read_with(cx, |v, _| v.navigator_working().is_empty()));
    let words: Vec<Option<String>> =
        view.read_with(cx, WorkspaceView::navigator_words).into_iter().map(|(_, w)| w).collect();
    assert!(words.contains(&Some("Working".to_owned())), "{words:?}");
    assert!(words.contains(&Some("Needs approval".to_owned())), "the state's word: {words:?}");
    assert!(!words.contains(&Some("Needs you".to_owned())), "not the section's: {words:?}");

    click(cx, leak(format!("nav-worker-{}", studio.key)));
    assert!(shown(cx, "nav-working"), "folded away, the section lists it");
    assert_eq!(view.read_with(cx, |v, _| v.navigator_working()), [busy]);
}

/// An agent at rest says what it last said, a lone word quoted, and no state: never "Idle".
/// Its age runs from when it came to rest, not from when its shell started.
#[gpui::test]
fn a_resting_agent_reads_its_last_word_and_its_age(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let _tile = opens_in(&view, cx, &studio, session, studio.me, 1, Some("/w/oss/slopty"));
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    let since_ms = u64::try_from(now.saturating_sub(120_000)).unwrap();
    view.update_in(cx, |v, _w, cx| {
        let rest = AgentEvent {
            status: AgentStatus::Idle,
            detail: Some("done".into()),
            since_ms: WallMs::from_millis(since_ms),
            ..blocked(session)
        };
        v.agent_event(rest, cx);
    });
    cx.run_until_parked();
    let lines = view.read_with(cx, WorkspaceView::navigator_lines);
    let (_, meta, age) = lines.first().expect("its row");
    assert_eq!(meta, "\u{201c}done\u{201d} \u{b7} oss/slopty", "{lines:#?}");
    assert!(!meta.contains("Idle"), "{meta:?}");
    assert_eq!(age.as_deref(), Some("2m"), "from its rest: {lines:#?}");
}

/// A phone's title bar has no tabs, so its drawer heads the list with *Workspaces*: a row per
/// workspace, and "New workspace", which goes to the empty one the layout keeps last. A
/// desktop's navigator has no such section. The active workspace is said by its name's tone,
/// not a fill: the focused tile's row is the list's one selection, so two rows never both read
/// as selected.
#[gpui::test]
fn a_phone_drawer_lists_the_workspaces(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let tile = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    assert!(!shown(cx, "nav-workspaces"), "the title bar's tabs say it on a desktop");
    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    let heading = cx.debug_bounds("nav-workspaces").expect("the section heads the drawer");
    let space = cx.debug_bounds("nav-space-0").expect("the workspace's row");
    let new = cx.debug_bounds("nav-new-space").expect("and a way to a new one");
    let worker = cx.debug_bounds(leak(format!("nav-worker-{}", studio.key))).expect("the worker");
    assert!(heading.bottom() <= space.top() && space.bottom() <= new.top(), "in that order");
    assert!(new.bottom() <= worker.top(), "above the workers");
    let plate = view.read_with(cx, |v, _| v.navigator_plate()).expect("the selection's plate");
    let focused = cx.debug_bounds(selector("nav-tile", tile.item)).expect("the focused row");
    assert!((plate.top() - focused.top()).abs() < px(0.5), "under the focused tile: {plate:?}");
    let fills = [Theme::default().surfaces.overlay, Theme::default().surfaces.raised]
        .map(|fill| gpui::Background::from(crate::colors::hsla(fill)));
    let filled = cx.update(|window, _| {
        let scale = window.scale_factor();
        let near = |a: f32, b: Pixels| f32::from(b).mul_add(-scale, a).abs() < 1.0;
        window.painted_quads().into_iter().any(|q| {
            fills.contains(&q.background)
                && near(q.bounds.origin.y.0, space.origin.y)
                && near(q.bounds.size.height.0, space.size.height)
        })
    });
    assert!(!filled, "the active workspace's row has no fill of its own");
    click(cx, "nav-new-space");
    let (active, last) = view.read_with(cx, |v, _| {
        (v.layout().active_workspace(), v.layout().workspaces().len().saturating_sub(1))
    });
    assert_eq!(active, last, "the empty workspace kept last");
}

/// A shell whose directory is in `repo` on `branch`, as its worker reports it.
fn in_repo(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    version: u64,
    (repo, cwd, branch): (&str, &str, &str),
) -> TileRef {
    let session = SessionId::new();
    let tile = opens_in(view, cx, fake, session, fake.me, version, Some(cwd));
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        let reported = SessionSummary {
            repo: Some(repo.to_owned()),
            branch: Some(branch.to_owned()),
            ..summary(session, Some(cwd))
        };
        v.session_opened(key, reported, cx);
    });
    cx.run_until_parked();
    tile
}

/// The palette's line groups the navigator by repository: a repository's shells from every
/// worker under one header, each row naming its worker, its directory below the repository
/// and its branch; the shells in none last. Two repositories of one name say where each is.
/// The lens is kept with the layout, and the palette's line then goes back by worker.
#[gpui::test]
fn the_repository_lens_groups_across_workers(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let here = in_repo(&view, cx, &studio, 1, ("/w/oss/slopty", "/w/oss/slopty/crates", "main"));
    let there = in_repo(&view, cx, &laptop, 1, ("/w/oss/slopty", "/w/oss/slopty", "stripes"));
    let fork = in_repo(&view, cx, &laptop, 2, ("/w/forks/slopty", "/w/forks/slopty", "main"));
    let loose = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 2, Some("/w/notes"));
    let lens = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.layout.navigator().lens);
    assert_eq!(lens(cx), NavLens::Workers, "by worker until asked");
    let line = view.read_with(cx, |v, _| v.lens_line().label);
    assert_eq!(line, navigator::BY_REPOSITORY);

    view.update_in(cx, |v, w, cx| v.toggle_navigator_lens(&ToggleNavigatorLens, w, cx));
    cx.run_until_parked();
    assert_eq!(lens(cx), NavLens::Repositories);
    assert!(!shown(cx, leak(format!("nav-worker-{}", studio.key))), "no worker headers");
    let order = view.read_with(cx, |v, _| v.navigator_tiles());
    assert_eq!(order.last(), Some(&loose), "the shell in no repository comes last: {order:?}");
    let group = |t: TileRef| order.iter().position(|o| *o == t).expect("listed");
    assert_eq!(group(here).abs_diff(group(there)), 1, "one checkout path, one group");
    assert!(shown(cx, "nav-repo-/w/oss/slopty") && shown(cx, "nav-repo-/w/forks/slopty"));
    assert!(shown(cx, "nav-repo-none"));
    let metas = view.read_with(cx, |v, _| v.navigator_metas());
    let meta = |tile: TileRef| {
        metas.iter().find(|(t, _)| *t == tile).map(|(_, m)| m.clone()).expect("a row")
    };
    assert_eq!(meta(here), "studio · crates · main", "worker, directory below, branch");
    assert_eq!(meta(there), "laptop · stripes", "at the repository's root");
    assert_eq!(meta(fork), "laptop · main");
    let labels = view.read_with(cx, |v, _| v.navigator_repo_labels());
    assert!(labels.contains(&"slopty, in w/oss".to_owned()), "{labels:?}");
    assert!(labels.contains(&"slopty, in w/forks".to_owned()), "{labels:?}");

    click(cx, "nav-repo-/w/oss/slopty");
    let order = view.read_with(cx, |v, _| v.navigator_tiles());
    assert!(!order.contains(&here) && !order.contains(&there), "folded");
    assert!(order.contains(&fork));

    let line = view.read_with(cx, |v, _| v.lens_line().label);
    assert_eq!(line, navigator::BY_WORKER);
    let saved = view.read_with(cx, |v, _| v.layout.save().navigator.lens);
    assert_eq!(saved, NavLens::Repositories, "kept with the layout");
}

/// The repository lens groups by what a repository is, not where it is cloned: two clones at
/// different paths on two workers are one block once their workers say they share an origin,
/// a third at one of those paths joins before its own identity comes, and a clone of another
/// repository of the same name says where it is. A fold is kept by the repository's key.
#[gpui::test]
fn the_repository_lens_groups_clones_by_their_identity(cx: &mut TestAppContext) {
    use slopty_proto::terminal::RepoId;

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let identified = |fake: &Fake, version, (repo, origin): (&str, Option<&str>), cx: &mut _| {
        let tile = in_repo(&view, cx, fake, version, (repo, repo, "main"));
        let session = view.read_with(cx, |v, _| session_of(v, tile));
        let key = fake.key;
        view.update_in(cx, |v, _w, cx| {
            let id = origin.map(|o| RepoId { origin: Some(o.to_owned()), ..RepoId::default() });
            let reported = SessionSummary {
                repo: Some(repo.to_owned()),
                repo_id: id,
                branch: Some("main".to_owned()),
                ..summary(session, Some(repo))
            };
            v.session_opened(key, reported, cx);
        });
        cx.run_until_parked();
        tile
    };
    let origin = "github.com/aislopware/slopty";
    let here = identified(&studio, 1, ("/w/oss/slopty", Some(origin)), cx);
    let there = identified(&laptop, 1, ("/Users/c/code/slopty", Some(origin)), cx);
    let unknown = identified(&studio, 2, ("/w/oss/slopty", None), cx);
    let fork = identified(&laptop, 2, ("/w/forks/slopty", Some("github.com/someone/slopty")), cx);
    view.update_in(cx, |v, w, cx| v.toggle_navigator_lens(&ToggleNavigatorLens, w, cx));
    cx.run_until_parked();

    let order = view.read_with(cx, |v, _| v.navigator_tiles());
    let at = |t: TileRef| order.iter().position(|o| *o == t).expect("listed");
    let mut one = [at(here), at(there), at(unknown)];
    one.sort_unstable();
    assert_eq!(one[0].abs_diff(one[2]), 2, "one repository, one block of three: {order:?}");
    assert!(at(fork) < one[0] || at(fork) > one[2], "the fork is a block of its own");
    assert!(shown(cx, leak(format!("nav-repo-{origin}"))), "kept by its origin");
    let labels = view.read_with(cx, |v, _| v.navigator_repo_labels());
    assert!(labels.contains(&"slopty, in github.com/aislopware".to_owned()), "{labels:?}");
    assert!(labels.contains(&"slopty, in github.com/someone".to_owned()), "{labels:?}");

    click(cx, leak(format!("nav-repo-{origin}")));
    let order = view.read_with(cx, |v, _| v.navigator_tiles());
    for tile in [here, there, unknown] {
        assert!(!order.contains(&tile), "folded with its repository");
    }
    assert!(order.contains(&fork));
}

/// An agent's subagents at work fold into its row as a count, and one that finished is not
/// counted: the rail lists the agent once, saying how many it has out.
#[gpui::test]
fn an_agents_subagents_at_work_fold_into_its_row_as_a_count(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::TableFrame;
    use slopty_proto::thread::{Cursor, Link, Phase, ThreadId};

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let root = state.row(WallMs::ZERO);
    let child = |phase: Phase| {
        let mut row = root.clone();
        row.id = ThreadId::new();
        row.terminal = None;
        row.parent =
            Some(Link { thread: root.id, item: slopty_proto::thread::ItemId("call".to_owned()) });
        row.status.phase = phase;
        row
    };
    let meta = |rows: Vec<slopty_proto::thread::wire::ThreadRow>, cx: &mut VisualTestContext| {
        let table = TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows };
        view.update_in(cx, |v, _w, cx| v.thread_table(key, &table, cx));
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            let item = v.item(tile).cloned().expect("the tile");
            v.tile_meta(&item, SystemTime::now(), cx).0
        })
    };
    let one = meta(vec![root.clone(), child(Phase::Working), child(Phase::Done)], cx);
    assert!(one.contains("1 subagent") && !one.contains("subagents"), "{one}");
    let two = meta(vec![root.clone(), child(Phase::Working), child(Phase::Waiting)], cx);
    assert!(two.contains("2 subagents"), "{two}");
    let none = meta(vec![root.clone(), child(Phase::Done)], cx);
    assert!(!none.contains("subagent"), "a finished one is not counted: {none}");
}

/// A row leads with its status glyph (an agent at work turns the working mark in the kind's
/// place), its working tree's changes sit right-aligned under the first line's end, and under
/// the pointer that end gives way to a close button, which takes the tile off.
#[gpui::test]
fn a_tile_row_leads_with_its_state_and_closes_from_under_the_pointer(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let keep = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 2);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        let summary = SessionSummary {
            repo: Some("/Users/me/oss/slopty".into()),
            branch: Some("main".into()),
            changes: Some(slopty_proto::terminal::RepoChanges { files: 2, added: 25, removed: 15 }),
            ..summary(session, Some("/Users/me/oss/slopty"))
        };
        v.session_opened(key, summary, cx);
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
    });
    click(cx, selector("nav-tile", keep.item));
    cx.simulate_mouse_move(point(px(1.0), px(1.0)), None, Modifiers::default());
    cx.run_until_parked();
    let id = tile.item.as_uuid();
    let kind = cx.debug_bounds(leak(format!("nav-kind-{id}"))).expect("the lead slot");
    let working = tree(cx).into_iter().any(|n| n.is("Image", Some("Working")));
    assert!(working, "the working mark leads the row");
    let (lines, changes) = (
        cx.debug_bounds(leak(format!("nav-lines-{id}"))).expect("the lines"),
        cx.debug_bounds(leak(format!("nav-changes-{id}"))).expect("the changes"),
    );
    assert!((kind.left() - lines.left()).abs() < px(0.5), "the glyph leads: {kind:?}");
    assert!((lines.right() - changes.right()).abs() < px(0.5), "right-aligned: {changes:?}");

    let close = leak(format!("nav-close-{}", keep.item.as_uuid()));
    assert!(!shown(cx, close), "the close waits for the pointer");
    let row = cx.debug_bounds(selector("nav-tile", keep.item)).expect("the row");
    cx.simulate_mouse_move(row.center(), None, Modifiers::default());
    cx.run_until_parked();
    assert!(shown(cx, close), "under the pointer it shows");
    studio.drain();
    click(cx, close);
    let sent = studio.drain();
    assert!(
        sent.iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Remove(i)) if *i == keep.item)),
        "it takes the tile off: {sent:?}"
    );
}

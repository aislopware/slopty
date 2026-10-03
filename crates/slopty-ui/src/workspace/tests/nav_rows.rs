//! The navigator's rows in the headless workspace: the filter, a tile's two lines, a folded
//! worker's rollup and the "+" on a worker's header.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gpui::Modifiers;
use slopty_client::groups::{self, GroupKey, fact};
use slopty_core::WallMs;

use super::super::actions::{GroupNavigatorBy, ToggleNavigatorLens};
use super::*;
use crate::icons::Status;

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

/// The filter keeps the tiles whose title, second line or project has what was typed, and every
/// tile of a worker whose name has it. A fold hides nothing while it filters; with nothing left
/// it says so; ↩ goes to the first tile listed, or the one ↑↓ walked to, and Esc empties it.
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
    let project = GroupKey::new(fact::FOLDER, &groups::at(studio.key, "/w/oss/slopty"));
    click(cx, leak(format!("nav-group-{project}")));
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

    click(cx, "nav-filter-clear");
    filter(cx, "studio");
    let listed = view.read_with(cx, |v, _| v.navigator_tiles());
    assert_eq!(listed.len(), 2, "{listed:?}");
    cx.simulate_keystrokes("down down");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.navigator_filter().to_owned()), "studio", "kept");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), listed.get(1).copied(), "↓↓ walked to the second");
    click(cx, "nav-filter-clear");
    filter(cx, "studio");
    cx.simulate_keystrokes("up enter");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), listed.last().copied(), "↑ starts from the end");
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
/// what its agent says, its directory below its project's root and its branch, in the order
/// every second line keeps. Once
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
        name: None,
        facts: BTreeMap::new(),
    };
    let tile = TileRef { worker: studio.key, item: item.id };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        let summary = SessionSummary {
            branch: Some("main".into()),
            changes: None,
            started_ms: WallMs::from_millis(u64::try_from(started).unwrap()),
            repo: Some("/Users/me/oss/slopty".into()),
            ..summary(session, Some("/Users/me/oss/slopty/crates"))
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
            "Run touch refused.txt \u{b7} crates \u{b7} main".to_owned(),
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
    let project = GroupKey::new(fact::REPO, &groups::at(key, "/Users/me/oss/slopty"));
    click(cx, leak(format!("nav-group-{project}")));
    assert!(shown(cx, "nav-needs-you"), "folded away, the section lists it");
    let mut part = |name: &str| {
        cx.debug_bounds(leak(format!("nav-waiting-{name}-{session}")))
            .unwrap_or_else(|| panic!("the agent row's {name}"))
    };
    let (words, separator, place) = (part("words"), part("separator"), part("place"));
    assert!((separator.left() - words.right()).abs() < px(0.5), "{words:?} {separator:?}");
    assert!((place.left() - separator.right()).abs() < px(0.5), "{separator:?} {place:?}");
    let spaced = cx.update(|window, _cx| {
        let font_size = px(Theme::default().typography.small());
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
    const THREADS: usize = 40;
    const BOARDS: usize = 5;
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
            name: None,
            facts: BTreeMap::new(),
        });
    }
    for _ in 0..NOTES {
        let kind = ItemKind::Note { text: "a note\n".into() };
        items.push(Item { id: ItemId::new(), kind, name: None, facts: BTreeMap::new() });
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
    // Its state, not its bounds: a release build keeps no debug selectors.
    let shown =
        |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.layout().navigator().shown);
    assert!(shown(cx));
    let docked = time(cx);
    // The action itself: a snapshot's tiles arrive unfocused, so no element holds the keyboard
    // for ⌘B to reach the workspace through.
    view.update_in(cx, |v, window, cx| {
        v.toggle_navigator(&ToggleNavigator, window, cx);
    });
    cx.run_until_parked();
    assert!(!shown(cx));
    let hidden = time(cx);
    println!(
        "MEASURE navigator, {SHELLS} shells + {NOTES} notes, {FRAMES} frames: docked p50 {:.3} \
         ms p95 {:.3} ms; hidden p50 {:.3} ms p95 {:.3} ms",
        docked.0, docked.1, hidden.0, hidden.1
    );

    // Then the declared projects' board rows and the threads with no tile here: half the
    // threads in a shell's project, half in a folder of their own.
    let rows: Vec<slopty_proto::thread::wire::ThreadRow> = (0..THREADS)
        .map(|n| {
            let mut row = crate::conversation::thread::fixtures::thread("edit").row(WallMs::ZERO);
            row.id = slopty_proto::thread::ThreadId::new();
            row.terminal = None;
            let cwd = if n % 2 == 0 {
                format!("/Users/me/src/project_{n}")
            } else {
                format!("/Users/me/elsewhere/thread_{n}")
            };
            row.cwd = Some(cwd);
            row
        })
        .collect();
    let boards = (0..BOARDS)
        .map(|n| {
            let mut board = crate::project::fixtures::project(&format!("board-{n}"), None);
            board.members = vec![
                [(fact::CWD.to_owned(), format!("/Users/me/src/project_{}", n.saturating_mul(3)))]
                    .into(),
            ];
            crate::project::fixtures::status(board, Vec::new(), Vec::new())
        })
        .collect();
    let table = slopty_proto::thread::wire::TableFrame::Snapshot {
        cursor: slopty_proto::thread::Cursor { epoch: 1, seq: 1 },
        rows,
    };
    view.update_in(cx, |v, window, cx| {
        v.threads_linked(key, cx);
        v.thread_table(key, &table, cx);
        v.projects_part(crate::project::fixtures::snapshot(1, boards), cx);
        v.toggle_navigator(&ToggleNavigator, window, cx);
    });
    cx.run_until_parked();
    assert!(shown(cx));
    let busy = time(cx);
    let mut looked = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        let start = Instant::now();
        let look = view.read_with(cx, |v, _| v.attention_look());
        looked.push(start.elapsed());
        drop(look);
    }
    looked.sort_unstable();
    let pct = |p: usize| slopty_client::pacing::percentile(&looked, p).as_secs_f64() * 1e3;
    println!(
        "MEASURE navigator with {THREADS} threads with no tile + {BOARDS} boards: docked p50 \
         {:.3} ms p95 {:.3} ms; attention look p50 {:.3} ms p95 {:.3} ms",
        busy.0,
        busy.1,
        pct(50),
        pct(95)
    );
}

/// A tile's two lines sit in the middle of its row at either density: at the touch height the
/// row grows round them, not under them.
#[gpui::test]
fn a_rows_two_lines_sit_in_its_middle(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let tile = in_repo(&view, cx, &studio, 1, ("/w/oss/app", "/w/oss/app/src", "main"));
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

/// An agent at rest says what it last said, and no state: never "Idle". A lone word is left to
/// its mark; at its project's root it has no place to add. Its age runs from when it came to rest,
/// not from when its shell started.
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
    assert_eq!(meta, "", "a lone word is the mark's to say: {lines:#?}");
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
    let fills = [Theme::default().surfaces.selected, Theme::default().surfaces.hover]
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
pub(super) fn in_repo(
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

/// A shell in a clone of `repo` at `cwd`, on `branch`, its worker having read its identity
/// as `origin` (none yet for `None`).
fn clone_of(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    version: u64,
    (repo, cwd, branch): (&str, &str, &str),
    origin: Option<&str>,
) -> TileRef {
    use slopty_proto::terminal::RepoId;
    let tile = in_repo(view, cx, fake, version, (repo, cwd, branch));
    let session = view.read_with(cx, |v, _| session_of(v, tile));
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        let id = origin.map(|o| RepoId { origin: Some(o.to_owned()), ..RepoId::default() });
        let reported = SessionSummary {
            repo: Some(repo.to_owned()),
            repo_id: id,
            branch: Some(branch.to_owned()),
            ..summary(session, Some(cwd))
        };
        v.session_opened(key, reported, cx);
    });
    cx.run_until_parked();
    tile
}

/// The key of the project a repository is by its origin.
fn repo_key(origin: &str) -> &'static str {
    leak(format!("nav-group-repo:{origin}"))
}

/// The navigator groups by project. One repository's clones on two workers are one project
/// (a clone whose identity has not come yet joins the one at its place), and its header says
/// the machines it spans; another repository of the same name says whose it is; a shell in a
/// plain folder is a project named by the folder; a shell in its home directory sits under its
/// worker in *Workers*, last. A fold is kept by the project's key.
#[gpui::test]
fn a_project_header_says_the_machines_it_spans(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let origin = "github.com/aislopware/slopty";
    let here = clone_of(
        &view,
        cx,
        &studio,
        1,
        ("/w/oss/slopty", "/w/oss/slopty/crates", "main"),
        Some(origin),
    );
    let there = clone_of(
        &view,
        cx,
        &laptop,
        1,
        ("/Users/c/slopty", "/Users/c/slopty", "stripes"),
        Some(origin),
    );
    let unknown = clone_of(&view, cx, &studio, 2, ("/w/oss/slopty", "/w/oss/slopty", "main"), None);
    let fork = clone_of(
        &view,
        cx,
        &laptop,
        2,
        ("/w/forks/slopty", "/w/forks/slopty", "main"),
        Some("github.com/someone/slopty"),
    );
    let notes = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 3, Some("/w/notes"));
    let home = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 3);

    let headings = view.read_with(cx, |v, _| v.navigator_headings());
    assert_eq!(headings, ["Projects", "Machines"]);
    let groups = view.read_with(cx, |v, _| v.navigator_groups());
    let spans =
        |key: &str| groups.iter().find(|(k, _)| k.as_str() == key).and_then(|(_, m)| m.clone());
    assert_eq!(spans(&format!("repo:{origin}")).as_deref(), Some("studio, laptop"));
    assert_eq!(spans("repo:github.com/someone/slopty").as_deref(), Some("laptop"));
    let labels = view.read_with(cx, |v, _| v.navigator_group_labels());
    assert!(labels.contains(&"slopty, in github.com/aislopware".to_owned()), "{labels:?}");
    assert!(labels.contains(&"slopty, in github.com/someone".to_owned()), "{labels:?}");
    assert!(labels.contains(&"notes".to_owned()), "a folder is a project of its own: {labels:?}");

    let order = view.read_with(cx, |v, _| v.navigator_tiles());
    let at = |t: TileRef| order.iter().position(|o| *o == t).expect("listed");
    let mut one = [at(here), at(there), at(unknown)];
    one.sort_unstable();
    assert_eq!(one[0].abs_diff(one[2]), 2, "one project, one block of three: {order:?}");
    assert!(at(fork) < one[0] || at(fork) > one[2], "the fork is a block of its own");
    assert_eq!(order.last(), Some(&home), "the home shell is its worker's, last: {order:?}");
    assert!(at(notes) < at(home));
    let worker = cx.debug_bounds(leak(format!("nav-worker-{}", laptop.key))).expect("laptop");
    let home_row = cx.debug_bounds(selector("nav-tile", home.item)).expect("the home shell");
    assert!(worker.bottom() <= home_row.top(), "under its worker");

    click(cx, repo_key(origin));
    let order = view.read_with(cx, |v, _| v.navigator_tiles());
    for tile in [here, there, unknown] {
        assert!(!order.contains(&tile), "folded with its project");
    }
    assert!(order.contains(&fork) && order.contains(&notes));
}

/// A row names its machine only where its project spans several: then its words, its machine,
/// its directory below the clone's root (nothing at the root) and its branch; a project on one
/// machine leaves the machine to its header.
#[gpui::test]
fn a_row_names_its_machine_only_where_its_project_spans_several(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let origin = "github.com/aislopware/slopty";
    let here = clone_of(
        &view,
        cx,
        &studio,
        1,
        ("/w/oss/slopty", "/w/oss/slopty/crates", "main"),
        Some(origin),
    );
    let there = clone_of(
        &view,
        cx,
        &laptop,
        1,
        ("/Users/c/slopty", "/Users/c/slopty", "stripes"),
        Some(origin),
    );
    let site = clone_of(
        &view,
        cx,
        &studio,
        2,
        ("/w/site", "/w/site/src", "main"),
        Some("github.com/o/site"),
    );
    let metas = view.read_with(cx, |v, _| v.navigator_metas());
    let meta = |tile: TileRef| {
        metas.iter().find(|(t, _)| *t == tile).map(|(_, m)| m.clone()).expect("a row")
    };
    assert_eq!(meta(here), "studio · crates · main", "machine, directory below, branch");
    assert_eq!(meta(there), "laptop · stripes", "at the clone's root");
    assert_eq!(meta(site), "src · main", "one machine: its header says where");
}

/// "Group the navigator by machine" brings back each worker's own block with every tile of it,
/// and the palette's line then goes back by project; the grouping is kept with the layout.
#[gpui::test]
fn group_by_machine_brings_back_the_workers_blocks(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let origin = "github.com/aislopware/slopty";
    let here =
        clone_of(&view, cx, &studio, 1, ("/w/oss/slopty", "/w/oss/slopty", "main"), Some(origin));
    let there = clone_of(
        &view,
        cx,
        &laptop,
        1,
        ("/Users/c/slopty", "/Users/c/slopty", "main"),
        Some(origin),
    );
    assert!(shown(cx, repo_key(origin)), "by project until asked");
    let lines = view.read_with(cx, |v, _| v.group_lines());
    assert_eq!(lines.first().map(|l| l.label.as_str()), Some(navigator::BY_MACHINE));

    view.update_in(cx, |v, w, cx| v.toggle_navigator_lens(&ToggleNavigatorLens, w, cx));
    cx.run_until_parked();
    assert!(!shown(cx, repo_key(origin)), "no project headers");
    let order = view.read_with(cx, |v, _| v.navigator_tiles());
    assert_eq!(order, [here, there], "each under its worker, in the workers' order");
    let studio_row = cx.debug_bounds(leak(format!("nav-worker-{}", studio.key))).expect("studio");
    let laptop_row = cx.debug_bounds(leak(format!("nav-worker-{}", laptop.key))).expect("laptop");
    let here_row = cx.debug_bounds(selector("nav-tile", here.item)).expect("here");
    assert!(studio_row.bottom() <= here_row.top() && here_row.bottom() <= laptop_row.top());
    let headings = view.read_with(cx, |v, _| v.navigator_headings());
    assert!(headings.is_empty(), "a lone section has no heading: {headings:?}");
    let lines = view.read_with(cx, |v, _| v.group_lines());
    assert_eq!(lines.first().map(|l| l.label.as_str()), Some(navigator::BY_PROJECT));
    let saved = view.read_with(cx, |v, _| v.layout.save().navigator.group_by);
    assert_eq!(saved, ["machine"], "kept with the layout");
}

/// Any fact a tile has is a grouping: "by branch" lists a group per branch, with what has no
/// branch under its worker; the palette offers it because a tile has a branch.
#[gpui::test]
fn any_fact_a_tile_has_is_a_grouping(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let main = in_repo(&view, cx, &studio, 1, ("/w/a", "/w/a", "main"));
    let fix = in_repo(&view, cx, &studio, 2, ("/w/b", "/w/b", "fix"));
    let note = arrives(&view, cx, &studio, ItemKind::Note { text: "plan\n".into() }, 3);
    let labels: Vec<String> =
        view.read_with(cx, |v, _| v.group_lines().into_iter().map(|l| l.label).collect());
    assert!(labels.iter().any(|l| l == "Group the navigator by branch"), "{labels:?}");
    assert!(!labels.iter().any(|l| l.ends_with("by cwd")), "a directory is no grouping");
    let chain = vec!["branch".to_owned(), "machine".to_owned()];
    view.update_in(cx, |v, w, cx| {
        v.group_navigator_by(&GroupNavigatorBy { chain }, w, cx);
    });
    cx.run_until_parked();
    assert!(shown(cx, "nav-group-branch:main") && shown(cx, "nav-group-branch:fix"));
    let headings = view.read_with(cx, |v, _| v.navigator_headings());
    assert_eq!(headings, ["Branches", "Machines"]);
    let order = view.read_with(cx, |v, _| v.navigator_tiles());
    assert_eq!(order, [fix, main, note], "by name, then what has no branch under its worker");
}

/// The filter takes facets: a machine by its name, a project by its name, any fact a tile has,
/// with words beside them; a facet nothing has leaves nothing.
#[gpui::test]
fn the_filter_takes_a_facet(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let a = in_repo(&view, cx, &studio, 1, ("/w/atlas", "/w/atlas", "main"));
    let b = in_repo(&view, cx, &laptop, 1, ("/w/atlas", "/w/atlas", "stripes"));
    let c = in_repo(&view, cx, &laptop, 2, ("/w/site", "/w/site", "main"));
    let listed = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.navigator_tiles());
    let narrowed = |cx: &mut VisualTestContext, text: &str| {
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        filter(cx, text);
        listed(cx)
    };
    assert_eq!(narrowed(cx, "machine:lapt"), [b, c], "a machine by its name");
    assert_eq!(narrowed(cx, "branch:main"), [a, c], "any fact a tile has");
    assert_eq!(narrowed(cx, "project:SITE"), [c], "a project by its name, any case");
    assert_eq!(narrowed(cx, "machine:studio atlas"), [a], "a facet and words together");
    assert_eq!(narrowed(cx, "branch:nowhere"), Vec::<TileRef>::new());
    assert!(shown(cx, "nav-nothing"), "and says so");
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

/// A worker that goes away keeps its tiles' rows where they were in their project, each marked
/// away, and its own row under *Workers* says why.
#[gpui::test]
fn an_away_worker_shows_on_its_projects_rows_and_in_workers(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let origin = "github.com/aislopware/slopty";
    let here = clone_of(&view, cx, &studio, 1, ("/w/slopty", "/w/slopty", "main"), Some(origin));
    let there = clone_of(&view, cx, &laptop, 1, ("/u/slopty", "/u/slopty", "main"), Some(origin));
    let before = view.read_with(cx, |v, _| v.navigator_tiles());
    let key = laptop.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.navigator_tiles()), before, "nothing moved");
    assert!(shown(cx, repo_key(origin)), "its project still lists it");
    let mark = view.read_with(cx, |v, _| {
        let item = v.item(there).cloned().expect("its item");
        v.tile_status(there, &item)
    });
    assert_eq!(mark, Some(Status::Away), "its row is marked away");
    let names: Vec<String> = tree(cx).into_iter().filter_map(|n| n.label).collect();
    assert!(names.iter().any(|l| l.starts_with("laptop, reconnecting")), "{names:#?}");
    assert!(before.contains(&here));
}

/// Hidden where it would dock, the navigator leaves a rail of its projects, each with what its
/// tiles add up to; a worker shows there only while it is not up. A project's glyph goes to it.
#[gpui::test]
fn the_rail_keeps_the_projects_in_view(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let origin = "github.com/aislopware/slopty";
    let session = SessionId::new();
    let here = opens_in(&view, cx, &studio, session, studio.me, 1, Some("/w/notes"));
    let there = clone_of(&view, cx, &laptop, 1, ("/u/slopty", "/u/slopty", "main"), Some(origin));
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    let notes = view.read_with(cx, |v, _| {
        let projects = v.project_groups();
        projects.group_of(here).map(|g| g.key.to_string()).expect("a project")
    });
    assert!(shown(cx, leak(format!("nav-rail-{notes}"))), "the folder's project");
    assert!(shown(cx, leak(format!("nav-rail-repo:{origin}"))), "the repository's");
    assert!(!shown(cx, leak(format!("nav-rail-{}", studio.key))), "a worker that is fine is not");
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    cx.run_until_parked();
    assert!(shown(cx, leak(format!("nav-rail-{notes}-rollup"))), "what it adds up to");
    let key = laptop.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
    assert!(shown(cx, leak(format!("nav-rail-{}", laptop.key))), "a worker away is");
    view.update_in(cx, |v, _w, cx| v.focus_tile(here, cx));
    cx.run_until_parked();
    click(cx, leak(format!("nav-rail-repo:{origin}")));
    assert_eq!(focused(&view, cx), Some(there), "a project's glyph goes to it");
}

/// More projects than the rail is tall scroll under the wheel, so the last is in reach.
#[gpui::test]
fn the_rail_scrolls_its_projects(cx: &mut TestAppContext) {
    use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase};
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let folders: Vec<String> = (0..40).map(|n| format!("/w/p{n:02}")).collect();
    let mut last = None;
    for (n, folder) in (1_u64..).zip(&folders) {
        last = Some(opens_in(&view, cx, &studio, SessionId::new(), studio.me, n, Some(folder)));
    }
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    let last = last.expect("opened");
    let key = view.read_with(cx, |v, _| {
        v.project_groups().group_of(last).map(|g| g.key.to_string()).expect("a project")
    });
    let glyph = leak(format!("nav-rail-{key}"));
    let rail = cx.debug_bounds("nav-rail").expect("the rail");
    let before = cx.debug_bounds(glyph).expect("laid out");
    assert!(before.bottom() > rail.bottom(), "past the foot: {before:?} {rail:?}");
    cx.simulate_event(ScrollWheelEvent {
        position: rail.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-100_000.0))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        momentum_phase: None,
    });
    cx.run_until_parked();
    let after = cx.debug_bounds(glyph).expect("laid out");
    assert!(after.bottom() <= rail.bottom() + px(0.5), "scrolled into reach: {after:?} {rail:?}");
}

/// A shell that moves to another checkout moves row to its new project at once; its tile stays
/// where it was in the strip.
#[gpui::test]
fn a_shell_that_changes_checkout_moves_row_not_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let atlas = in_repo(&view, cx, &studio, 1, ("/w/atlas", "/w/atlas", "main"));
    let site = in_repo(&view, cx, &studio, 2, ("/w/site", "/w/site", "main"));
    let at = view.read_with(cx, |v, _| v.layout().position(site));
    let session = view.read_with(cx, |v, _| session_of(v, site));
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        let moved = SessionSummary {
            repo: Some("/w/atlas".to_owned()),
            branch: Some("main".to_owned()),
            ..summary(session, Some("/w/atlas/docs"))
        };
        v.session_opened(key, moved, cx);
    });
    cx.run_until_parked();
    let groups = view.read_with(cx, |v, _| v.navigator_groups());
    assert_eq!(groups.len(), 1, "one project left: {groups:?}");
    let order = view.read_with(cx, |v, _| v.navigator_tiles());
    assert_eq!(order, [atlas, site], "the moved shell's row joined atlas");
    assert_eq!(view.read_with(cx, |v, _| v.layout().position(site)), at, "its tile did not move");
}

//! An agent terminal's thread face in the workspace: the toggle, what it follows, the marks and
//! names its thread gives the tile, and which started threads open as tiles.

use slopty_core::WallMs;
use slopty_proto::thread::wire::{
    Intent, IntentDone, Outcome, RequestCard, TableFrame, ThreadFrame, ThreadRequest, ThreadRow,
};
use slopty_proto::thread::{AskId, Cursor, Phase, Request, ThreadMeta, ThreadState};

use super::*;
use crate::icons::Status;
use crate::workspace::faces::Face;

/// A shell of this client's on `fake`'s worker with Claude Code working in it, focused.
fn agent_tile(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &mut Fake,
) -> (TileRef, SessionId) {
    let session = SessionId::new();
    let tile = opens(view, cx, fake, session, fake.me, 1);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    fake.drain();
    (tile, session)
}

/// A thread whose TUI runs in `session`.
fn thread_on(session: SessionId) -> ThreadState {
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    state
}

/// `rows` as `key`'s whole thread table, its link up.
fn table(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    key: WorkerKey,
    rows: Vec<ThreadRow>,
) {
    let table = TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows };
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        v.thread_table(key, &table, cx);
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// What the workspace asked of the worker about threads.
fn thread_requests(fake: &mut Fake) -> Vec<ThreadRequest> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            // The starts' own ask, as the link comes up, for the folders past sessions ran in.
            ClientMsg::Thread(ThreadRequest::Sessions { .. }) => None,
            ClientMsg::Thread(req) => Some(req),
            _ => None,
        })
        .collect()
}

fn follows(requests: &[ThreadRequest], thread: slopty_proto::thread::ThreadId) -> bool {
    requests.iter().any(|r| matches!(r, ThreadRequest::Follow { thread: t, .. } if *t == thread))
}

fn face_shown(view: &Entity<WorkspaceView>, cx: &VisualTestContext, session: SessionId) -> bool {
    view.read_with(cx, |v, _| v.face_shown(session))
}

/// ⌘J swaps the tile between its thread and the TUI over the same session: hiding the thread
/// lets it go and gives the terminal the keyboard back, showing it follows it again with the
/// keyboard in its composer, and a draft survives the round.
#[gpui::test]
fn the_face_toggles_over_the_same_session_and_keeps_its_draft(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (tile, session) = agent_tile(&view, cx, &mut studio);
    assert!(!face_shown(&view, cx, session), "the TUI until a thread is known");
    let state = thread_on(session);
    let thread = state.meta.id;
    table(&view, cx, studio.key, vec![state.row(WallMs::ZERO)]);
    assert!(face_shown(&view, cx, session), "the thread once known");
    assert!(follows(&thread_requests(&mut studio), thread));
    cx.simulate_input("half a thought");
    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    assert!(!face_shown(&view, cx, session));
    assert!(thread_requests(&mut studio).contains(&ThreadRequest::Unfollow { thread }));
    assert!(terminal_focused(&view, cx, session), "the TUI takes the keyboard back");
    assert!(cx.debug_bounds("thread-composer").is_none());

    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(follows(&thread_requests(&mut studio), thread), "followed again");
    let draft = view.read_with(cx, |v, cx| v.thread_face(session).map(|t| t.read(cx).draft(cx)));
    assert_eq!(draft.as_deref(), Some("half a thought"), "the draft waited");
    assert_eq!(focused(&view, cx), Some(tile));
    cx.simulate_input(", whole");
    cx.run_until_parked();
    let draft = view.read_with(cx, |v, cx| v.thread_face(session).map(|t| t.read(cx).draft(cx)));
    assert_eq!(draft.as_deref(), Some("half a thought, whole"), "the keyboard is the thread's");
}

/// The header's face toggle, under the pointer, shows the other face per click and is named for
/// it; a shell no agent runs in has none.
#[gpui::test]
fn the_toggle_picks_the_face_and_a_plain_shell_has_none(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let plain = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    let (tile, session) = agent_tile(&view, cx, &mut studio);
    table(&view, cx, studio.key, vec![thread_on(session).row(WallMs::ZERO)]);
    let shows = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.tile_face(session));
    assert_eq!(shows(cx), Face::Thread);
    let hover = |cx: &mut VisualTestContext, tile: TileRef| {
        let header = cx.debug_bounds(selector("title", tile.item)).expect("its header");
        cx.simulate_mouse_move(header.center(), None, Modifiers::none());
        cx.run_until_parked();
    };
    hover(cx, plain);
    assert!(cx.debug_bounds(selector("close", plain.item)).is_some(), "its controls show");
    for face in [Face::Thread, Face::Terminal] {
        let part = selector(&format!("face-{}", face.key()), plain.item);
        assert!(cx.debug_bounds(part).is_none(), "a shell has one face");
    }

    let click = |cx: &mut VisualTestContext, face: Face| {
        hover(cx, tile);
        let named = format!("Show {}", face.label().to_lowercase());
        assert!(tree(cx).iter().any(|n| n.is("Button", Some(&named))), "named {named}");
        let part = selector(&format!("face-{}", face.key()), tile.item);
        let at = cx.debug_bounds(part).unwrap_or_else(|| panic!("{face:?} on the toggle"));
        cx.simulate_click(at.center(), Modifiers::none());
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    };
    click(cx, Face::Terminal);
    assert_eq!(shows(cx), Face::Terminal);
    assert!(terminal_focused(&view, cx, session), "the TUI takes the keyboard");
    click(cx, Face::Thread);
    assert_eq!(shows(cx), Face::Thread);
    assert!(cx.debug_bounds("thread-composer").is_some());
}

/// The agent leaving the terminal lets its thread go and gives the TUI the keyboard; back, the
/// pick stands; closing the tile lets it go again.
#[gpui::test]
fn closing_the_tile_or_the_agent_leaving_lets_the_thread_go(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (tile, session) = agent_tile(&view, cx, &mut studio);
    let state = thread_on(session);
    let thread = state.meta.id;
    table(&view, cx, studio.key, vec![state.row(WallMs::ZERO)]);
    assert!(follows(&thread_requests(&mut studio), thread));
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::None, ..blocked(session) }, cx);
    });
    cx.run_until_parked();
    assert!(thread_requests(&mut studio).contains(&ThreadRequest::Unfollow { thread }));
    assert!(terminal_focused(&view, cx, session), "the keyboard goes back to the TUI");

    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
    });
    cx.run_until_parked();
    assert!(follows(&thread_requests(&mut studio), thread), "the thread again");
    view.update_in(cx, |v, _w, cx| {
        let key = studio.key;
        v.apply_sync(
            key,
            ItemSync::Delta { version: 2, by: ClientId::new(), op: ItemOp::Remove(tile.item) },
            cx,
        );
    });
    cx.run_until_parked();
    assert!(thread_requests(&mut studio).contains(&ThreadRequest::Unfollow { thread }));
}

/// An agent's pill says the state alone ("Needs approval"), since what the agent asks is the
/// navigator's line and the pointer's; a screen reader still hears all of it. On a phone the
/// tile has no header: the bar is the tile's, its heading the title and all the pill would say.
#[gpui::test]
fn a_pill_says_the_state_and_a_phone_bar_says_it_all(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    cx.run_until_parked();
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    paired(&view, cx, &studio, tile, 2);
    let asks = AgentEvent {
        status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
        detail: Some("$ touch a-file-with-a-rather-long-name-for-a-phone.txt".into()),
        ..blocked(session)
    };
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(asks, cx);
        // On its TUI, where the header's pill speaks for it.
        v.show_face(session, false, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    let width = |cx: &mut VisualTestContext| {
        let name = cx.debug_bounds(selector("name", tile.item)).expect("the title");
        let pill = cx.debug_bounds(selector("agent", tile.item)).expect("the pill");
        let header = cx.debug_bounds(selector("title", tile.item)).expect("the header");
        assert!(pill.left() >= name.right(), "side by side: {name:?} {pill:?}");
        assert!(pill.right() <= header.right(), "inside the header: {pill:?} {header:?}");
        f32::from(pill.size.width)
    };
    let word = width(cx);
    let full = "Needs approval: Run touch a-file-with-a-rather-long-name-for-a-phone.txt";
    assert!(tree(cx).iter().any(|n| n.label.as_deref() == Some(full)), "all of it, said");
    let brief = AgentEvent {
        status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }),
        detail: Some("$ ls".into()),
        ..blocked(session)
    };
    view.update_in(cx, |v, _w, cx| v.agent_event(brief, cx));
    cx.run_until_parked();
    assert!((width(cx) - word).abs() < 0.5, "the word, not the ask");

    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("title", tile.item)).is_none(), "no header on a phone");
    let heading = tree(cx).into_iter().find(|n| {
        n.role == "Heading"
            && n.label.as_deref().is_some_and(|l| l.ends_with("Needs approval: Run ls"))
    });
    assert!(heading.is_some(), "the bar says all of it: {:#?}", tree(cx));
}

/// While the agent's thread says a turn runs, its tile is marked working though the worker's
/// hook still says idle, and its row's second line does not say "Idle"; once the thread rests
/// the mark follows the hook again. The thread folds in the transcript; it drives nothing.
#[gpui::test]
fn a_thread_mid_turn_marks_its_tile_working_while_the_hook_lags(cx: &mut TestAppContext) {
    use std::time::SystemTime;

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (tile, session) = agent_tile(&view, cx, &mut studio);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Idle, ..blocked(session) }, cx);
    });
    let mut state = thread_on(session);
    state.status.phase = Phase::Idle;
    table(&view, cx, studio.key, vec![state.row(WallMs::ZERO)]);
    let mark = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.item(tile).and_then(|item| v.tile_status(tile, item)))
    };
    assert_eq!(mark(cx), Some(Status::Idle), "the hook's word");

    state.status.phase = Phase::Working;
    let mut row = state.row(WallMs::ZERO);
    row.last_line = Some("Running **the tests**".to_owned());
    table(&view, cx, studio.key, vec![row]);
    assert_eq!(mark(cx), Some(Status::Working), "the thread knows better");
    let said = view.read_with(cx, |v, cx| {
        v.item(tile).map(|item| v.tile_meta(item, SystemTime::now(), cx).0).unwrap_or_default()
    });
    assert!(!said.contains("Idle"), "{said:?}");
    let line = view.read_with(cx, |v, _| v.face_summary(session));
    assert_eq!(line.as_deref(), Some("Running the tests"), "its last line, said plainly");

    state.status.phase = Phase::Idle;
    table(&view, cx, studio.key, vec![state.row(WallMs::ZERO)]);
    assert_eq!(mark(cx), Some(Status::Idle), "at rest: the hook's word again");
}

/// While the thread shows a request in its tray, that is the tile's statement: the header
/// wears no pill saying the same thing above it. Over the TUI the pill is back.
#[gpui::test]
fn the_request_is_said_once(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (tile, session) = agent_tile(&view, cx, &mut studio);
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    let mut row = thread_on(session).row(WallMs::ZERO);
    row.requests = vec![RequestCard {
        id: AskId("ask-1".to_owned()),
        item: None,
        kind: Request::APPROVAL.to_owned(),
        title: "Run `cargo test`".to_owned(),
        options: Vec::new(),
        opened_ms: WallMs::ZERO,
    }];
    table(&view, cx, studio.key, vec![row]);
    assert!(view.read_with(cx, |v, _| v.face_asks(session)), "the thread asks");
    assert!(cx.debug_bounds(selector("agent", tile.item)).is_none(), "no pill over it");
    view.update_in(cx, |v, _w, cx| v.show_face(session, false, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("agent", tile.item)).is_some(), "the TUI's header says it");
}

/// Two agents that have not titled themselves read alike, "Claude Code" and "Claude Code 2",
/// until one's thread is titled; that one is then named by it and the other loses its number.
#[gpui::test]
fn an_untitled_agent_is_named_by_its_thread(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (first, one) = agent_tile(&view, cx, &mut studio);
    let two = SessionId::new();
    let second = opens(&view, cx, &studio, two, studio.me, 2);
    view.update_in(cx, |v, _w, cx| {
        for session in [one, two] {
            v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        }
    });
    cx.run_until_parked();
    let titles = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| [first, second].map(|t| v.tile_title(v.item(t).unwrap())))
    };
    assert_eq!(titles(cx), ["Claude Code", "Claude Code 2"]);
    let mut row = thread_on(one).row(WallMs::ZERO);
    row.title = "Fix the flaky test".to_owned();
    let other = view.read_with(cx, |v, cx| v.row_of_terminal(studio.key, two, cx)).expect("two's");
    table(&view, cx, studio.key, vec![row, other]);
    assert_eq!(titles(cx), ["Fix the flaky test", "Claude Code"]);
}

/// An agent tile whose terminal the worker's thread table names opens on its thread view on a
/// Mac too: the link brings the table and the thread's frames into it, a dropped link keeps
/// what it showed, and a new one catches up from where it stood.
#[gpui::test]
fn an_agent_with_a_thread_opens_on_its_thread_view(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (_tile, session) = agent_tile(&view, cx, &mut studio);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    assert_eq!(thread_requests(&mut studio), [ThreadRequest::Table { have: None }]);
    assert!(!face_shown(&view, cx, session), "the TUI until a thread is known");

    let state = thread_on(session);
    let thread = state.meta.id;
    table(&view, cx, key, vec![state.row(WallMs::ZERO)]);
    assert!(face_shown(&view, cx, session), "face-first once the agent has a thread");
    let follows_now = thread_requests(&mut studio);
    assert!(
        follows_now.iter().any(
            |r| matches!(r, ThreadRequest::Follow { thread: t, have: None, .. } if *t == thread)
        ),
        "its thread view follows it: {follows_now:?}"
    );
    let face = view.read_with(cx, |v, _| v.thread_face(session).cloned()).expect("a thread view");
    let snapshot =
        ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 40 }, state: Box::new(state) };
    view.update_in(cx, |v, _w, cx| v.thread_frame(key, thread, snapshot, cx));
    cx.run_until_parked();
    assert!(!face.read_with(cx, |f, _| f.rows().is_empty()), "the thread is in its rows");
    let light = Theme::new(slopty_theme::Variant::Light);
    view.update(cx, |v, cx| v.set_theme(light.clone(), cx));
    assert_eq!(face.read_with(cx, |f, _| f.theme().clone()), light, "it follows the theme");

    view.update_in(cx, |v, _w, cx| v.threads_unlinked(key, cx));
    cx.run_until_parked();
    assert!(!face.read_with(cx, |f, _| f.rows().is_empty()), "what it showed stays");
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let again = thread_requests(&mut studio);
    assert!(
        again
            .iter()
            .any(|r| matches!(r, ThreadRequest::Follow { have: Some(Cursor { seq: 40, .. }), .. })),
        "caught up from where it stood: {again:?}"
    );
}

/// An agent's subagents are the table's rows whose chain of parents reaches its thread, however
/// deep; another agent's are not, nor a chain that loops.
#[gpui::test]
fn an_agent_s_subagents_are_the_rows_under_its_thread(cx: &mut TestAppContext) {
    use slopty_proto::thread::{ItemId, Link, ThreadId};

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (_tile, session) = agent_tile(&view, cx, &mut studio);
    let key = studio.key;

    let mut root = crate::conversation::thread::fixtures::empty();
    root.meta.terminal = Some(session);
    let row = |parent: Option<ThreadId>| {
        let mut state = crate::conversation::thread::fixtures::empty();
        state.meta.parent = parent.map(|thread| Link { thread, item: ItemId("call".to_owned()) });
        state.row(WallMs::ZERO)
    };
    let child = row(Some(root.meta.id));
    let grandchild = row(Some(child.id));
    let other = row(Some(ThreadId::new()));
    let mut looping = row(None);
    looping.parent = Some(Link { thread: looping.id, item: ItemId("call".to_owned()) });
    let rows = vec![root.row(WallMs::ZERO), child.clone(), grandchild.clone(), other, looping];
    table(&view, cx, key, rows);

    let mut found: Vec<ThreadId> =
        view.read_with(cx, |v, cx| v.subagents(session, cx).into_iter().map(|r| r.id).collect());
    found.sort();
    let mut want = vec![child.id, grandchild.id];
    want.sort();
    assert_eq!(found, want);
}

/// A thread the worker started from another (a branch) opens as a tile of its own; an aside
/// stays in the sheet of the view that asked it, and its row is nobody's: no tile, no terminal's
/// thread, no wait counted.
#[gpui::test]
fn a_branch_opens_as_a_tile_and_an_aside_does_not(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let (_tile, session) = agent_tile(&view, cx, &mut studio);
    let key = studio.key;
    let mut state = thread_on(session);
    state.meta.caps.push(slopty_proto::thread::Cap::named(slopty_proto::thread::Cap::FORK));
    let thread = state.meta.id;
    table(&view, cx, key, vec![state.row(WallMs::ZERO)]);
    let started = |cx: &mut VisualTestContext, intent: Intent| {
        let made = slopty_proto::thread::ThreadId::new();
        view.update_in(cx, |v, _w, cx| {
            let hub = v.thread_hub(key, cx);
            let id = hub.update(cx, |hub, cx| hub.intent(thread, intent, cx));
            let done = IntentDone { id, outcome: Outcome::Started { thread: made } };
            v.thread_done(key, &done, cx);
        });
        cx.run_until_parked();
        made
    };

    let branch = started(cx, Intent::Fork { after: None });
    assert!(view.read_with(cx, |v, _| v.tile_of_thread(branch)).is_some(), "the branch's tile");

    let aside = started(cx, Intent::Aside);
    assert!(view.read_with(cx, |v, _| v.tile_of_thread(aside)).is_none(), "no tile for an aside");
    let mut asked = crate::conversation::thread::fixtures::empty();
    asked.meta.id = aside;
    asked.meta.terminal = Some(session);
    asked.meta.facts.insert(ThreadMeta::ASIDE_FACT.to_owned(), thread.to_string());
    let mut aside_row = asked.row(WallMs::ZERO);
    aside_row.requests = vec![RequestCard {
        id: AskId("ask-1".to_owned()),
        item: None,
        kind: Request::APPROVAL.to_owned(),
        title: "Run `ls`".to_owned(),
        options: Vec::new(),
        opened_ms: WallMs::ZERO,
    }];
    table(&view, cx, key, vec![state.row(WallMs::ZERO), aside_row]);
    view.read_with(cx, |v, _| {
        assert_eq!(v.session_thread(session), Some(thread), "the terminal's thread stays its own");
        assert!(v.thread_request(aside).is_none(), "no row of its own in the chrome");
        assert_eq!(v.needs_you_count(), 0, "its request is the sheet's");
    });
}

/// A thread goes on on another machine that has a clone of its repository: the machines that
/// do (by its origin, the clone itself rather than a worktree of it, the thread's folder
/// within it) are offered, one that has another repository is not, and going on opens a
/// start there whose composer holds the pointer back, nothing sent until the person sends it.
#[gpui::test]
fn a_thread_goes_on_on_another_machine_with_its_repository(cx: &mut TestAppContext) {
    use slopty_proto::terminal::RepoId;

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let mut forge = connect(&view, cx, 2, "forge");
    let attic = connect(&view, cx, 3, "attic");
    let repo = |origin: &str| RepoId { origin: Some(origin.to_owned()), root: None, url: None };
    let atlas = repo("github.com/o/atlas");
    let checkout = |root: &str, id: &RepoId| SessionSummary {
        repo: Some(root.to_owned()),
        repo_id: Some(id.clone()),
        ..summary(SessionId::new(), Some(root))
    };
    view.update_in(cx, |v, _w, cx| {
        v.session_opened(forge.key, checkout("/home/f/atlas/.claude/worktrees/try-it", &atlas), cx);
        v.session_opened(forge.key, checkout("/home/f/atlas", &atlas), cx);
        v.session_opened(attic.key, checkout("/srv/other", &repo("github.com/o/other")), cx);
    });
    let (_tile, session) = agent_tile(&view, cx, &mut studio);
    let state = thread_on(session);
    let thread = state.meta.id;
    let mut row = state.row(WallMs::ZERO);
    row.cwd = Some("/w/atlas/crates/x".to_owned());
    row.repo = Some("/w/atlas".to_owned());
    row.repo_id = Some(atlas.clone());
    table(&view, cx, studio.key, vec![row]);

    let found = view.read_with(cx, |v, _| v.going_on_elsewhere(studio.key, thread));
    let places: Vec<(&str, &str)> =
        found.iter().map(|e| (e.machine.as_str(), e.cwd.as_str())).collect();
    assert_eq!(places, [("forge", "/home/f/atlas/crates/x")]);
    assert_eq!(found[0].agents.first(), Some(&state.meta.agent), "the thread's own agent leads");

    let there = found[0].clone();
    let pointer = "This goes on from thread …".to_owned();
    view.update_in(cx, |v, _w, cx| {
        v.continue_on(there.worker, there.agents[0].clone(), there.cwd, pointer.clone(), cx);
    });
    cx.run_until_parked();
    let start = focused(&view, cx).expect("the start has the focus");
    assert_eq!(start.worker, forge.key, "on the other machine");
    let (cwd, drafted) = view.update(cx, |v, cx| {
        let cwd = v.starting.get(start.item).map(|s| s.cwd.clone());
        (cwd, v.starting.draft_view(start.item).map(|d| d.read(cx).draft(cx)))
    });
    assert_eq!(cwd.as_deref(), Some("/home/f/atlas/crates/x"));
    assert_eq!(drafted, Some(pointer), "the pointer waits in its composer");
    assert!(thread_starts(&mut forge).is_empty(), "nothing goes until the person sends it");
}

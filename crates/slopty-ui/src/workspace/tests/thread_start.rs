//! "New agent…", the start by steps: the agent, then the machine, then the folder, each step
//! listing the last choice first and passed over with one choice; the palette's "New `agent`
//! agent" lines starting at the machine; the start sent to that machine, and the thread it
//! answers with opened as a tile of its own.

use slopty_core::WorkerId;
use slopty_proto::thread::wire::{IntentDone, Outcome, Start, ThreadRequest};
use slopty_proto::thread::{AgentId, ThreadId};

use super::super::actions::{NewAgent, NewAgentOf, ResumeSession, StartThread};
use super::super::agent_start::{READING_SESSIONS, RESUME_PAST};
use super::super::projects::worker_key;
use super::*;
use crate::palette::PaletteRun;

/// A machine's capabilities with `agents` installed: what its link says it can start.
fn with_agents(agents: &[AgentId]) -> WorkerCaps {
    let installed = agents
        .iter()
        .map(|agent| slopty_proto::server::InstalledAgent {
            agent: agent.clone(),
            version: "1.0".to_owned(),
            offers: slopty_proto::thread::Offers::default(),
        })
        .collect();
    WorkerCaps { agents: installed, ..healthy() }
}

/// The palette's "New … agent" labels, in order.
fn agent_labels(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.update(cx, |v, cx| v.palette_lines(cx))
        .into_iter()
        .filter(|l| {
            matches!(&l.run, PaletteRun::Action(a)
                if a.as_any().downcast_ref::<NewAgentOf>().is_some())
        })
        .map(|l| l.label)
        .collect()
}

/// The lines of the palette that is up, in order.
fn step_lines(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, cx| {
        v.palette.clone().map(|p| p.read(cx).matches().iter().map(|l| l.label.clone()).collect())
    })
    .unwrap_or_default()
}

/// The thread starts `fake` was sent, as (intent, agent, folder, prompt).
fn starts(
    fake: &mut Fake,
) -> Vec<(slopty_proto::thread::IntentId, AgentId, String, Option<String>)> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { id, start }) => {
                Some((id, start.agent, start.cwd, start.prompt))
            }
            _ => None,
        })
        .collect()
}

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// A studio with Claude Code and Codex, and a laptop with Claude Code, each as its own link
/// says, with no server; a shell in `/src/app` on the studio has the focus.
struct Two {
    studio: Fake,
    laptop: Fake,
}

fn two_machines(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Two {
    let (studio_id, laptop_id) = (WorkerId::new(), WorkerId::new());
    let studio = connect(view, cx, worker_key(studio_id).value(), "studio");
    let laptop = connect(view, cx, worker_key(laptop_id).value(), "laptop");
    let shell = opens_in(view, cx, &studio, SessionId::new(), studio.me, 1, Some("/src/app"));
    let (claude, codex) = (AgentId::named(AgentId::CLAUDE_CODE), AgentId::named(AgentId::CODEX));
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(studio.key, with_agents(&[claude.clone(), codex]), cx);
        v.set_worker_caps(laptop.key, with_agents(&[claude]), cx);
        v.focus_tile(shell, cx);
    });
    cx.run_until_parked();
    Two { studio, laptop }
}

/// "New agent…" asks which agent, then (where several can start it) which machine, then which
/// folder, the focused shell's first; ↩ on each starts it. The next "New agent…" lists that agent,
/// machine and folder first, so ↩ ↩ starts the same again.
#[gpui::test]
fn new_agent_opens_the_picker_with_the_last_choices(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    studio.drain();

    cx.dispatch_action(NewAgent);
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["Claude Code", "Codex"], "which agent");
    // Each agent's line carries its own mark beside its name: the name is the choice.
    let marks: Option<Vec<_>> = view.read_with(cx, |v, cx| {
        v.palette.clone().map(|p| p.read(cx).matches().iter().map(|l| l.icon).collect())
    });
    let (claude, codex) = (crate::icons::AgentMark::Claude, crate::icons::AgentMark::Blossom);
    assert_eq!(marks, Some(vec![Some(claude.into()), Some(codex.into())]), "each its own mark");
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(
        step_lines(&view, cx),
        ["src/app", "~", RESUME_PAST],
        "only the studio has Codex: the folder"
    );
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert!(starts(&mut studio).is_empty(), "nothing goes before the first message");
    cx.simulate_input("fix the flaky test");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent = starts(&mut studio);
    let [(_, agent, cwd, Some(prompt))] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!((agent, cwd.as_str()), (&AgentId::named(AgentId::CODEX), "/src/app"));
    assert_eq!(prompt, "fix the flaky test", "the first message goes as the start's prompt");

    cx.dispatch_action(NewAgent);
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["Codex", "Claude Code"], "the last agent first");
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent = starts(&mut studio);
    let [(_, agent, cwd, None)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!((agent, cwd.as_str()), (&AgentId::named(AgentId::CODEX), "/src/app"), "again");
}

/// The palette's "New `agent` agent" lines, one per agent any machine can start, skip the
/// agent step: Claude Code asks which machine (the focused one first), and the laptop's folder
/// step offers its home. The start goes to the laptop.
#[gpui::test]
fn per_agent_lines_skip_the_agent_step(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let Two { mut studio, mut laptop } = two_machines(&view, cx);
    assert_eq!(agent_labels(&view, cx), ["New Claude Code agent", "New Codex agent"]);
    studio.drain();
    laptop.drain();

    let claude = AgentId::named(AgentId::CLAUDE_CODE);
    cx.dispatch_action(NewAgentOf { agent: claude.clone() });
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["studio", "laptop"], "which machine, the focus's first");
    cx.simulate_input("laptop");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["~", RESUME_PAST], "the laptop has no shell: its home");
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent = starts(&mut laptop);
    let [(_, agent, cwd, None)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!((agent, cwd.as_str()), (&claude, "~"));
    assert!(starts(&mut studio).is_empty(), "nothing went to the studio");
}

/// With no server, each machine's own link says what it can start: Claude Code on both, so
/// "New agent…" passes the agent step and asks which machine. A machine whose link found nothing
/// offers nothing, and with none at all "New agent…" says so.
#[gpui::test]
fn a_start_with_no_server_lists_each_machines_agents(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _laptop = connect(&view, cx, 2, "laptop");
    let shell = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/src/app"));
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    assert_eq!(agent_labels(&view, cx), ["New Claude Code agent"]);
    cx.dispatch_action(NewAgent);
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["studio", "laptop"], "one agent: which machine");
    cx.simulate_keystrokes("escape");
    settle(cx);
}

/// A machine whose link found no agent offers none, and with none anywhere "New agent…" says so.
#[gpui::test]
fn with_no_agent_anywhere_new_agent_says_so(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let caps = WorkerCaps { agents: Vec::new(), ..healthy() };
    view.update_in(cx, |v, _w, cx| v.set_worker_caps(studio.key, caps, cx));
    cx.run_until_parked();
    assert_eq!(agent_labels(&view, cx), Vec::<String>::new());
    cx.dispatch_action(NewAgent);
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()).as_deref(), Some(agent_start::NO_AGENT));
}

/// The tile of the newest thread on its way on `key`, and what it says.
fn starting_tile(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Option<TileRef> {
    view.read_with(cx, |v, _| v.focused().filter(|t| v.starting.has(t.item)))
}

/// A start shows its tile at once, saying it is starting, and the machine's answer opens the
/// thread in that same tile, which takes the keyboard. A refusal is said in the machine's
/// words and takes the tile away; so does an answer that is no thread.
#[gpui::test]
fn a_started_thread_opens_as_a_tile_and_a_refusal_is_said(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let studio_key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(studio_key, cx));
    cx.run_until_parked();
    studio.drain();
    let claude = AgentId::named(AgentId::CLAUDE_CODE);
    view.update_in(cx, |v, _w, cx| {
        v.start_thread(studio_key, claude.clone(), "~".into(), None, cx);
    });
    let sent = starts(&mut studio);
    let [(intent, ..)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    settle(cx);
    let placeholder = starting_tile(&view, cx).expect("the start's tile, before any answer");
    let selector = format!("starting-{}", placeholder.item.as_uuid());
    let says = cx.debug_bounds(selector.leak()).is_some();
    assert!(says, "it says the agent is starting");

    let thread = ThreadId::new();
    let started = IntentDone { id: *intent, outcome: Outcome::Started { thread } };
    view.update_in(cx, |v, _w, cx| v.thread_done(studio_key, &started, cx));
    settle(cx);
    let added: Vec<Item> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(item)) => Some(item),
            _ => None,
        })
        .collect();
    let [item] = added.as_slice() else { panic!("one thread item: {added:?}") };
    assert_eq!(item.kind, ItemKind::Thread { thread });
    assert_eq!(item.id, placeholder.item, "the thread fills the start's own tile");

    let by = studio.me;
    let op = ItemOp::Add(item.clone());
    view.update_in(cx, |v, _w, cx| {
        v.apply_sync(studio_key, ItemSync::Delta { version: 2, by, op }, cx);
    });
    settle(cx);
    let tile = TileRef { worker: studio_key, item: item.id };
    assert_eq!(focused(&view, cx), Some(tile), "the thread comes to the front");
    let keyboard = view.update_in(cx, |v, window, cx| {
        v.thread_item(item.id)
            .is_some_and(|t| Focusable::focus_handle(t.read(cx), cx).is_focused(window))
    });
    assert!(keyboard, "its thread view holds the keyboard");
    assert_eq!(view.read_with(cx, |v, _| v.tile_title(item)), "Thread");

    view.update_in(cx, |v, _w, cx| v.start_thread(studio_key, claude, "~".into(), None, cx));
    let sent = starts(&mut studio);
    let [(intent, ..)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    let refused = IntentDone {
        id: *intent,
        outcome: Outcome::Refused { reason: "claude is not on this machine's PATH".into() },
    };
    settle(cx);
    let refused_tile = starting_tile(&view, cx).expect("the second start's tile");
    view.update_in(cx, |v, _w, cx| v.thread_done(studio_key, &refused, cx));
    settle(cx);
    let gone = view.read_with(cx, |v, _| !v.layout.contains(refused_tile));
    assert!(gone, "a refused start's tile goes");
    assert!(
        !studio.drain().iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Add(_)))),
        "a refusal adds no tile"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("claude is not on this machine's PATH")
    );

    let codex = AgentId::named(AgentId::CODEX);
    view.update_in(cx, |v, _w, cx| v.start_thread(studio_key, codex, "~".into(), None, cx));
    let sent = starts(&mut studio);
    let [(intent, ..)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    let unsupported = slopty_proto::thread::Cap::named(slopty_proto::thread::Cap::HANDOFF);
    let unsupported =
        IntentDone { id: *intent, outcome: Outcome::Unsupported { cap: unsupported } };
    view.update_in(cx, |v, _w, cx| v.thread_done(studio_key, &unsupported, cx));
    settle(cx);
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("studio can\u{2019}t start Codex"),
        "a start the machine cannot make is said, not dropped"
    );
}

/// A palette opened before the machine's link says it has Codex takes it as it arrives: what
/// was typed finds the new line, and ↩ starts at the machine step, with no closing and opening
/// it again.
#[gpui::test]
fn an_open_palette_takes_the_agents_as_they_arrive(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let shell = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/src/app"));
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    let studio_key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(studio_key, cx));
    cx.run_until_parked();
    studio.drain();

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("n e w space c o d e x space a g e n t");
    cx.run_until_parked();
    let agents = [AgentId::named(AgentId::CLAUDE_CODE), AgentId::named(AgentId::CODEX)];
    view.update_in(cx, |v, _w, cx| v.set_worker_caps(studio_key, with_agents(&agents), cx));
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["src/app", "~", RESUME_PAST], "one machine: the folder");
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent = starts(&mut studio);
    let [(_, agent, ..)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!(agent, &AgentId::named(AgentId::CODEX), "the line that arrived, chosen");
}

/// A machine the "+" menu chose first is not asked again: its agents, then its folders.
#[gpui::test]
fn the_plus_menus_machine_is_not_asked_again(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let Two { mut laptop, .. } = two_machines(&view, cx);
    laptop.drain();
    let key = laptop.key;
    view.update_in(cx, |v, window, cx| {
        v.new_on = Some(key);
        v.new_agent(&NewAgent, window, cx);
    });
    settle(cx);
    assert_eq!(
        step_lines(&view, cx),
        ["~", RESUME_PAST],
        "the laptop's one agent: straight to its folders"
    );
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent = starts(&mut laptop);
    assert_eq!(sent.len(), 1, "started on the laptop: {sent:?}");
}

/// A start's tile waits for its first message with nothing sent, and ⌘W takes it away with
/// nothing started. Once sent, a link that goes takes the tile with it and says so, since the
/// answer may never come; a tile still waiting for its first message stays.
#[gpui::test]
fn a_start_on_its_way_closes_and_goes_with_its_link(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    let key = studio.key;
    studio.drain();
    let codex = AgentId::named(AgentId::CODEX);
    view.update_in(cx, |v, window, cx| {
        let start = StartThread {
            worker: key,
            agent: codex.clone(),
            cwd: "/src/app".into(),
            worktree: false,
        };
        v.begin_start(start, window, cx);
    });
    settle(cx);
    let waiting = starting_tile(&view, cx).expect("the start's tile, waiting for its message");
    assert!(starts(&mut studio).is_empty(), "nothing sent while it waits");
    cx.simulate_keystrokes("cmd-w");
    settle(cx);
    assert!(!view.read_with(cx, |v, _| v.layout.contains(waiting)), "⌘W took it away");
    assert!(starts(&mut studio).is_empty(), "and started nothing");

    view.update_in(cx, |v, window, cx| {
        let start = StartThread {
            worker: key,
            agent: codex.clone(),
            cwd: "/src/app".into(),
            worktree: false,
        };
        v.begin_start(start, window, cx);
    });
    settle(cx);
    let unsent = starting_tile(&view, cx).expect("a tile waiting for its message");
    view.update_in(cx, |v, _w, cx| v.start_thread(key, codex, "~".into(), None, cx));
    settle(cx);
    let sent = starting_tile(&view, cx).expect("a start sent");
    assert_eq!(starts(&mut studio).len(), 1);
    view.update_in(cx, |v, _w, cx| v.threads_unlinked(key, cx));
    settle(cx);
    let (sent_gone, unsent_kept) =
        view.read_with(cx, |v, _| (!v.layout.contains(sent), v.layout.contains(unsent)));
    assert!(sent_gone, "a sent start goes with its link");
    assert!(unsent_kept, "a start not sent keeps its field");
    let said = view.read_with(cx, |v, _| v.toast_text()).unwrap_or_default();
    assert!(said.starts_with("studio went out of reach before Codex started"), "{said}");
}

/// A worker's thread hub knows what that machine can start ("Continue in…"): from its link's
/// caps when the hub is made, again when the caps change, and nothing once the link drops.
#[gpui::test]
fn a_workers_hub_knows_the_agents_it_can_start(cx: &mut TestAppContext) {
    use slopty_proto::server::InstalledAgent;
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let agents = |cx: &mut VisualTestContext| view.read_with(cx, |v, cx| v.hub_agents_of(key, cx));
    let claude = AgentId::named(AgentId::CLAUDE_CODE);
    assert_eq!(agents(cx), Some(vec![claude.clone()]), "from the link's caps");

    let mut caps = healthy();
    caps.agents.push(InstalledAgent {
        agent: AgentId::named(AgentId::CODEX),
        version: "0.50".into(),
        offers: slopty_proto::thread::Offers::default(),
    });
    view.update_in(cx, |v, _w, cx| v.set_worker_caps(key, caps, cx));
    assert_eq!(agents(cx), Some(vec![claude, AgentId::named(AgentId::CODEX)]), "caps moved");

    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    assert_eq!(agents(cx), Some(Vec::new()), "out of reach, it starts nothing");
}

/// "Resume a past session…" ends the folder step: the machine is asked for the agent's
/// sessions and the step opens at once saying it reads them, then lists them, found by their
/// prompts too. A session with no thread here starts its agent on it in its own words; one
/// whose kept thread runs opens that thread's tile. An answer nothing waits on is dropped.
#[gpui::test]
fn a_past_session_is_found_and_taken_up_again(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::{PastSession, PastSessions, PromptHit};

    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    let codex = AgentId::named(AgentId::CODEX);
    let key = studio.key;
    let session =
        |native: &str, title: Option<&str>, prompt: &str, thread: Option<ThreadId>| PastSession {
            agent: codex.clone(),
            native: native.to_owned(),
            cwd: Some("/src/app".to_owned()),
            title: title.map(str::to_owned),
            updated_ms: None,
            thread,
            resume: vec!["resume".to_owned(), native.to_owned()],
            facts: BTreeMap::new(),
            prompts: vec![PromptHit {
                text: prompt.to_owned(),
                spans: Vec::new(),
                cut_before: false,
                cut_after: false,
                at_ms: None,
            }],
        };
    let answer = |sessions: Vec<PastSession>| PastSessions {
        agent: Some(codex.clone()),
        cwd: None,
        query: String::new(),
        sessions,
        absent: None,
        cut: None,
    };
    view.update_in(cx, |v, _w, cx| v.past_sessions(key, answer(Vec::new()), cx));
    studio.drain();

    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_input("resume");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let asked: Vec<_> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Sessions { agent, cwd, query, .. }) => {
                Some((agent, cwd, query))
            }
            _ => None,
        })
        .collect();
    let ask = (Some(codex.clone()), None, String::new());
    assert_eq!(asked, [ask], "the folder step's ask, still out, is the session step's");
    assert_eq!(step_lines(&view, cx), Vec::<String>::new());
    assert!(cx.debug_bounds("palette-empty").is_some(), "{READING_SESSIONS}");

    let running = ThreadId::new();
    view.update_in(cx, |v, _w, cx| {
        let sessions = vec![
            session("019a", Some("Fix the parser"), "the parser drops a token", None),
            session("019b", None, "port the CLI to clap 5", Some(running)),
        ];
        v.past_sessions(key, answer(sessions), cx);
    });
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["Fix the parser", "port the CLI to clap 5"]);
    cx.simulate_input("token");
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["Fix the parser"], "found by its prompt");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent: Vec<_> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some((start.cwd, start.args)),
            _ => None,
        })
        .collect();
    assert_eq!(
        sent,
        [("/src/app".to_owned(), vec!["resume".to_owned(), "019a".to_owned()])],
        "its agent's own words take it up again"
    );

    let mut row = crate::conversation::thread::fixtures::thread("edit").row(WallMs::ZERO);
    row.id = running;
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        let table = slopty_proto::thread::wire::TableFrame::Snapshot {
            cursor: slopty_proto::thread::Cursor { epoch: 1, seq: 1 },
            rows: vec![row],
        };
        v.thread_table(key, &table, cx);
    });
    let pick = ResumeSession {
        worker: key,
        session: Box::new(session("019b", None, "port the CLI", Some(running))),
    };
    view.update_in(cx, |v, window, cx| v.resume_session(&pick, window, cx));
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.tile_of_thread(running)).is_some(), "its tile");
    assert!(starts(&mut studio).is_empty(), "a session that runs is not started twice");
}

/// The session step's words find the listed sessions at once, and once the field rests the
/// machine is asked for them: the sessions whose prompts it finds join the list, an answer for
/// words the field no longer says is dropped, the line the person is on stays chosen, and with
/// the words gone the list is the machine's own again.
#[gpui::test]
fn a_past_session_is_found_by_what_was_asked_in_it(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::{PastSession, PastSessions, PromptHit};

    use crate::conversation::thread::find::ASK_AFTER;

    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    let codex = AgentId::named(AgentId::CODEX);
    let key = studio.key;
    let session = |native: &str, prompt: &str| PastSession {
        agent: codex.clone(),
        native: native.to_owned(),
        cwd: Some("/src/app".to_owned()),
        title: None,
        updated_ms: None,
        thread: None,
        resume: vec!["resume".to_owned(), native.to_owned()],
        facts: BTreeMap::new(),
        prompts: vec![PromptHit {
            text: prompt.to_owned(),
            spans: Vec::new(),
            cut_before: false,
            cut_after: false,
            at_ms: None,
        }],
    };
    let answer = |query: &str, sessions: Vec<PastSession>| PastSessions {
        agent: Some(codex.clone()),
        cwd: None,
        query: query.to_owned(),
        sessions,
        absent: None,
        cut: None,
    };
    let asked = |studio: &mut Fake| -> Vec<String> {
        studio
            .drain()
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Thread(ThreadRequest::Sessions { query, .. }) => Some(query),
                _ => None,
            })
            .collect()
    };
    let chosen = |cx: &VisualTestContext| {
        view.read_with(cx, |v, cx| {
            let palette = v.palette.clone().expect("the step is up");
            palette.read(cx).chosen().map(|l| l.label.clone())
        })
    };

    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_input("resume");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(asked(&mut studio), [""], "the list, with no words: one ask for both steps");
    view.update_in(cx, |v, _w, cx| {
        let listed = vec![
            session("019a", "the parser drops a token"),
            session("019b", "port the CLI to clap 5"),
        ];
        v.past_sessions(key, answer("", listed), cx);
    });
    settle(cx);

    // A burst of keys finds the listed session at once and asks the machine once it rests.
    cx.simulate_input("cla");
    cx.executor().advance_clock(ASK_AFTER.div_f32(2.0));
    cx.simulate_input("p");
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["port the CLI to clap 5"], "the listed one, at once");
    assert_eq!(asked(&mut studio), Vec::<String>::new(), "nothing before the field rests");
    cx.executor().advance_clock(ASK_AFTER);
    settle(cx);
    assert_eq!(asked(&mut studio), ["clap"]);
    assert_eq!(chosen(cx).as_deref(), Some("port the CLI to clap 5"));

    // An answer for words the field no longer says is dropped.
    view.update_in(cx, |v, _w, cx| {
        v.past_sessions(key, answer("cla", vec![session("0100", "clamp the scroll")]), cx);
    });
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["port the CLI to clap 5"], "a stale answer is dropped");

    // The machine's answer adds a session older than the list; the chosen line stays chosen.
    view.update_in(cx, |v, _w, cx| {
        let found =
            vec![session("0042", "move to clap 4"), session("019b", "port the CLI to clap 5")];
        v.past_sessions(key, answer("clap", found), cx);
    });
    settle(cx);
    let mut lines = step_lines(&view, cx);
    lines.sort();
    assert_eq!(lines, ["move to clap 4", "port the CLI to clap 5"], "found by its prompt");
    assert_eq!(chosen(cx).as_deref(), Some("port the CLI to clap 5"), "the chosen line stays");

    // With the words gone, the list is the machine's own again, and nothing more is asked.
    for _ in 0..4 {
        cx.simulate_keystrokes("backspace");
    }
    cx.executor().advance_clock(ASK_AFTER);
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["the parser drops a token", "port the CLI to clap 5"]);
    assert_eq!(asked(&mut studio), Vec::<String>::new());

    // A machine out of reach once the field rests is not asked.
    cx.simulate_input("parser");
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.executor().advance_clock(ASK_AFTER);
    settle(cx);
    assert_eq!(asked(&mut studio), Vec::<String>::new(), "out of reach, it is not asked");
}

/// A start's first message is written in the thread's own composer: it keeps several lines,
/// pasted or broken by ⇧↵, and its chips choose what the machine offers a new thread of the
/// agent: the model and the mode go with the prompt in the start, and the hand-built plan-mode
/// flag is gone. An agent the machine offers nothing for shows no mode to choose, and the
/// palette has no "Start in plan mode" line.
#[gpui::test]
fn a_start_is_written_in_the_threads_composer(cx: &mut TestAppContext) {
    use slopty_proto::thread::{Mode, Model, Offers};

    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    let agents = [AgentId::named(AgentId::CLAUDE_CODE), AgentId::named(AgentId::CODEX)];
    let offers = Offers {
        models: ["opus", "sonnet"]
            .map(|id| Model { id: id.to_owned(), label: id.to_uppercase() })
            .to_vec(),
        modes: [("default", "Default"), ("plan", "Plan")]
            .map(|(id, label)| Mode {
                id: id.to_owned(),
                label: label.to_owned(),
                description: None,
            })
            .to_vec(),
        ..Offers::default()
    };
    let mut caps = with_agents(&agents);
    caps.agents[0].offers = offers;
    view.update_in(cx, |v, _w, cx| v.set_worker_caps(studio.key, caps, cx));
    cx.run_until_parked();
    studio.drain();
    let sent = |studio: &mut Fake| -> Vec<Start> {
        studio
            .drain()
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some(*start),
                _ => None,
            })
            .collect()
    };
    let click = |id: &'static str, cx: &mut VisualTestContext| {
        let at = cx.debug_bounds(id).unwrap_or_else(|| panic!("{id}")).center();
        cx.simulate_click(at, Modifiers::none());
        settle(cx);
    };
    // The agent, then (Claude Code is on both machines) the machine, then the folder.
    let begin = |agent: &str, steps: usize, cx: &mut VisualTestContext| {
        cx.dispatch_action(NewAgent);
        settle(cx);
        cx.simulate_input(agent);
        for _ in 0..steps {
            cx.simulate_keystrokes("enter");
            settle(cx);
        }
    };

    begin("codex", 2, cx);
    assert!(cx.debug_bounds("thread-hero").is_some(), "the new thread asks what to do there");
    click("thread-attach", cx);
    assert!(cx.debug_bounds("thread-add-menu-files").is_some(), "the + menu is open");
    assert!(cx.debug_bounds("thread-add-menu-modes").is_none(), "Codex: no mode offered");
    // A press outside the menu closes it, and the keyboard is the composer's again.
    let tile = view.read_with(cx, |v, _| v.focused()).expect("the start's tile");
    let title: &'static str = format!("title-{}", tile.item.as_uuid()).leak();
    click(title, cx);
    assert!(cx.debug_bounds("thread-add-menu-files").is_none(), "and closed");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let codex_start = sent(&mut studio);
    let [bare] = codex_start.as_slice() else { panic!("one start: {codex_start:?}") };
    assert_eq!((bare.prompt.as_deref(), bare.mode.as_deref()), (None, None), "started bare");

    begin("claude", 3, cx);
    let offered = view.update(cx, |v, cx| v.palette_lines(cx));
    assert!(!offered.iter().any(|l| l.label == "Start in plan mode"), "no plan line");
    cx.simulate_input("Plan the parser.\nKeep the lexer.");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("Then stop.");
    click("thread-model", cx);
    click("thread-menu-0", cx);
    click("thread-attach", cx);
    click("thread-add-menu-modes", cx);
    click("thread-menu-1", cx);
    assert!(sent(&mut studio).is_empty(), "nothing goes before ↵");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let claude_start = sent(&mut studio);
    let [start] = claude_start.as_slice() else { panic!("one start: {claude_start:?}") };
    assert_eq!(start.prompt.as_deref(), Some("Plan the parser.\nKeep the lexer.\nThen stop."));
    assert_eq!((start.model.as_deref(), start.mode.as_deref()), (Some("opus"), Some("plan")));
    assert!(start.args.is_empty(), "the mode is the start's, not a flag built here");
    assert!(cx.debug_bounds("thread-starting").is_some(), "it says it is starting");
}

/// A worktree's name is its first message's words, as a branch reads, with the tile's id's
/// end; with no words, its agent's name.
#[test]
fn a_worktree_is_named_by_its_first_message() {
    use super::super::starting::worktree_name;

    let item = ItemId::new();
    let id = item.as_uuid().simple().to_string();
    let tail = id.get(id.len() - 4..).unwrap_or_default().to_owned();
    let claude = AgentId::named(AgentId::CLAUDE_CODE);
    let named = |prompt: Option<&str>| worktree_name(prompt, &claude, item);
    assert_eq!(
        named(Some("Fix the login redirect, please!")),
        format!("fix-the-login-redirect-please-{tail}")
    );
    assert_eq!(
        named(Some("Add caching to the API layer for the slow endpoints today")),
        format!("add-caching-to-the-api-layer-{tail}"),
        "six words at most"
    );
    assert_eq!(named(Some("   ")), format!("claude-code-{tail}"));
    assert_eq!(named(None), format!("claude-code-{tail}"));
    let long = named(Some("supercalifragilisticexpialidocious antidisestablishmentarianism"));
    assert!(long.len() <= 40 + 5, "{long}");
    assert_eq!(
        named(Some("Sửa lỗi đăng nhập trên iPad")),
        format!("sua-loi-dang-nhap-tren-ipad-{tail}"),
        "Latin letters as ASCII"
    );
    assert_eq!(named(Some("Straße, Ørsted, Łódź")), format!("strasse-orsted-lodz-{tail}"));
    assert_eq!(
        named(Some("su\u{31b}\u{309}a lo\u{302}\u{303}i")),
        format!("sua-loi-{tail}"),
        "decomposed as typed, the same"
    );
    assert_eq!(
        named(Some("修复登录 重定向")),
        format!("修复登录-重定向-{tail}"),
        "a script with no Latin form keeps its letters"
    );
    assert_eq!(named(Some("แก้ไข")), format!("แก้ไข-{tail}"), "its marks with them");
    let wide = named(Some(&"修".repeat(30)));
    assert!(wide.len() <= 40 + 5 && wide.starts_with("修修"), "cut at a letter's edge: {wide}");
}

/// The folder step offers a new worktree of each repository its folders are in, once each,
/// after the folders; picking it starts the agent in that folder with a worktree of its own,
/// named by the first message, which the machine makes from the folder's clone.
#[gpui::test]
fn a_start_can_take_a_new_worktree_of_a_repository(cx: &mut TestAppContext) {
    use super::super::agent_start::NEW_WORKTREE;

    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    for (version, cwd) in [(2, "/w/atlas/web"), (3, "/w/atlas")] {
        let session = SessionId::new();
        let item = Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            name: None,
            facts: BTreeMap::new(),
        };
        let summary =
            SessionSummary { repo: Some("/w/atlas".to_owned()), ..summary(session, Some(cwd)) };
        let (key, by) = (studio.key, studio.me);
        view.update_in(cx, |v, _window, cx| {
            v.session_opened(key, summary, cx);
            v.apply_sync(key, ItemSync::Delta { version, by, op: ItemOp::Add(item) }, cx);
        });
    }
    cx.run_until_parked();
    studio.drain();

    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let lines = step_lines(&view, cx);
    let worktree = format!("{NEW_WORKTREE} atlas");
    let at = |line: &str| lines.iter().position(|l| l == line);
    assert_eq!(lines.iter().filter(|l| l.starts_with(NEW_WORKTREE)).count(), 1, "{lines:?}");
    assert!(at(&worktree) > at("~") && at(&worktree) < at(RESUME_PAST), "{lines:?}");

    cx.simulate_input("new worktree");
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_input("try the other layout");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent: Vec<Start> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some(*start),
            _ => None,
        })
        .collect();
    let [start] = sent.as_slice() else { panic!("one start: {sent:?}") };
    let worktree = start.worktree.as_ref().expect("a worktree");
    assert_eq!(worktree.base, None, "from the branch the clone has checked out");
    let name = worktree.name.as_str();
    let suffix = name.strip_prefix("try-the-other-layout-").expect("named by its first words");
    assert!(suffix.len() == 4 && suffix.chars().all(|c| c.is_ascii_hexdigit()), "{name}");
    assert_eq!(start.cwd, "/w/atlas", "the first folder in it, the most recent shell's");
}

/// One message on several agents: a start in a new worktree offers the machine's other agents
/// in its "+" menu, and each one ticked runs the same message too, in a new worktree of its
/// own, its tile a column of its own right of the first. The question says who will, the
/// worktrees share the message's words, and the keyboard stays with the first.
#[gpui::test]
fn one_message_starts_on_several_agents_each_in_a_worktree(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    let session = SessionId::new();
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::Terminal { session },
        name: None,
        facts: BTreeMap::new(),
    };
    let summary =
        SessionSummary { repo: Some("/w/atlas".to_owned()), ..summary(session, Some("/w/atlas")) };
    let (key, by) = (studio.key, studio.me);
    view.update_in(cx, |v, _window, cx| {
        v.session_opened(key, summary, cx);
        v.apply_sync(key, ItemSync::Delta { version: 2, by, op: ItemOp::Add(item) }, cx);
    });
    cx.run_until_parked();
    studio.drain();

    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_input("new worktree");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let click = |cx: &mut VisualTestContext, what: &str| {
        let at = cx.debug_bounds(Box::leak(what.to_owned().into_boxed_str())).expect(what);
        cx.simulate_click(at.center(), Modifiers::none());
        settle(cx);
    };
    click(cx, "thread-attach");
    click(cx, "thread-add-menu-also-claude-code");
    cx.update(|window, _| window.set_a11y_active(true));
    settle(cx);
    let tree = cx.update(|window, _| crate::a11y::tree(window));
    let asked = "What should Codex and Claude Code each do in a new worktree of atlas?";
    let heads: Vec<_> =
        tree.iter().filter(|n| n.role == "Heading").map(|n| n.label.clone()).collect();
    assert!(
        tree.iter().any(|n| n.is("Heading", Some(asked))),
        "who will, in the question: {heads:?}"
    );
    let first = view.read_with(cx, |v, _| v.focused()).expect("the draft's tile");

    cx.simulate_input("try the other layout");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent: Vec<Start> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some(*start),
            _ => None,
        })
        .collect();
    let agents: Vec<&str> = sent.iter().map(|s| s.agent.0.as_str()).collect();
    assert_eq!(agents, [AgentId::CODEX, AgentId::CLAUDE_CODE], "the first, then the other");
    let names: Vec<&str> =
        sent.iter().filter_map(|s| Some(s.worktree.as_ref()?.name.as_str())).collect();
    assert_eq!(names.len(), 2, "each in a worktree: {sent:?}");
    assert!(names.iter().all(|n| n.starts_with("try-the-other-layout-")), "{names:?}");
    assert_ne!(names[0], names[1], "of its own");
    assert!(sent.iter().all(|s| s.prompt.as_deref() == Some("try the other layout")));
    assert!(sent.iter().all(|s| s.cwd == "/w/atlas"));
    let (focused, panes) = view.read_with(cx, |v, _| {
        let tab = v.layout.shown_tab().expect("a tab on show");
        let first_pane = tab.pane_of(first);
        let others = tab.tiles().filter(|t| *t != first && tab.pane_of(*t) != first_pane).count();
        (v.focused(), others)
    });
    assert_eq!(focused, Some(first), "the keyboard stays with the first");
    assert_eq!(panes, 1, "the other run in a pane of its own beside it");
}

/// The place chip as a control: a draft in a repository's folder asks the machine for its
/// branches as it opens, and once they come its place chip switches it to a new worktree of
/// that folder, whose base chip lists the branches, the default first. The start goes in a new
/// worktree from the branch picked, and the question says so. Back in the folder, the pick
/// goes. A draft opened while the threads' link is down asks once it comes up.
#[gpui::test]
fn the_place_chip_starts_a_worktree_from_a_branch_picked(cx: &mut TestAppContext) {
    use slopty_proto::git::{Branch, Branches, GitDone, GitOp, GitOutcome};

    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    let session = SessionId::new();
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::Terminal { session },
        name: None,
        facts: BTreeMap::new(),
    };
    let summary =
        SessionSummary { repo: Some("/w/atlas".to_owned()), ..summary(session, Some("/w/atlas")) };
    let (key, by) = (studio.key, studio.me);
    view.update_in(cx, |v, _window, cx| {
        v.session_opened(key, summary, cx);
        v.apply_sync(key, ItemSync::Delta { version: 2, by, op: ItemOp::Add(item) }, cx);
    });
    cx.run_until_parked();
    studio.drain();

    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(step_lines(&view, cx).first().map(String::as_str), Some("w/atlas"), "the folder");
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.update(|window, _| window.set_a11y_active(true));
    settle(cx);
    let roles = |cx: &mut VisualTestContext, label: &str| -> Vec<String> {
        let tree = cx.update(|window, _| crate::a11y::tree(window));
        tree.iter().filter(|n| n.label.as_deref() == Some(label)).map(|n| n.role.clone()).collect()
    };
    let place = "Place, in atlas on studio";
    assert_eq!(roles(cx, place), ["Label"], "words, until the branches come");
    let reads = |fake: &mut Fake| {
        let msgs = fake.drain();
        msgs.iter().filter(|m| matches!(m, ClientMsg::Git { op: GitOp::Branches, .. })).count()
    };
    assert_eq!(reads(&mut studio), 0, "nothing goes while the threads' link is down");
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    settle(cx);
    let asked: Vec<_> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Git { request, repo, op: GitOp::Branches } => Some((request, repo)),
            _ => None,
        })
        .collect();
    let [(request, repo)] = asked.as_slice() else { panic!("one read: {asked:?}") };
    assert_eq!(repo, "/w/atlas", "of the draft's folder");
    let branch = |name: &str, local, committed| Branch {
        name: name.to_owned(),
        local,
        remote: true,
        committed,
    };
    let branches = Branches {
        current: Some("feature".to_owned()),
        default: Some("main".to_owned()),
        list: vec![
            branch("feature", true, 30),
            branch("develop", false, 20),
            branch("main", true, 10),
        ],
        more: 0,
    };
    let done = GitOutcome::Done(GitDone::Branches(Box::new(branches)));
    view.update_in(cx, |v, _w, cx| v.git_done(key, *request, done, cx));
    settle(cx);
    assert_eq!(roles(cx, place), ["Button"], "a switch once they came");
    assert!(cx.debug_bounds("thread-base").is_none(), "no base in the folder itself");
    assert_eq!(roles(cx, "Branch, feature"), ["Label"], "but the branch checked out, as words");
    let ledge = cx.debug_bounds("thread-ledge").expect("the composer's ledge");
    let chip = cx.debug_bounds("thread-place").expect("the place");
    assert!(ledge.contains(&chip.center()), "where the work is, on the ledge over the field");

    let click = |cx: &mut VisualTestContext, what: &str| {
        let at = cx.debug_bounds(Box::leak(what.to_owned().into_boxed_str())).expect(what);
        cx.simulate_click(at.center(), Modifiers::none());
        settle(cx);
    };
    click(cx, "thread-place");
    click(cx, "thread-menu-1");
    assert_eq!(roles(cx, "Base branch, feature"), ["Button"], "from the one checked out");
    click(cx, "thread-base");
    let tree = cx.update(|window, _| crate::a11y::tree(window));
    let listed: Vec<_> =
        tree.iter().filter(|n| n.role == "ListBoxOption").filter_map(|n| n.label.clone()).collect();
    assert_eq!(listed, ["main", "feature", "develop"], "the default, the checked out, the rest");
    click(cx, "thread-menu-2");
    let asked = "What should Codex do in a new worktree of atlas from develop?";
    let tree = cx.update(|window, _| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Heading", Some(asked))), "the question says where");
    // Back in the folder the base goes with the worktree; a new worktree again starts from
    // the branch checked out until one is picked.
    click(cx, "thread-place");
    click(cx, "thread-menu-0");
    assert!(cx.debug_bounds("thread-base").is_none(), "no base in the folder itself");
    click(cx, "thread-place");
    click(cx, "thread-menu-1");
    assert_eq!(roles(cx, "Base branch, feature"), ["Button"], "the pick went with it");
    click(cx, "thread-base");
    click(cx, "thread-menu-2");

    cx.simulate_input("port the parser");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent: Vec<Start> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some(*start),
            _ => None,
        })
        .collect();
    let [start] = sent.as_slice() else { panic!("one start: {sent:?}") };
    let worktree = start.worktree.as_ref().expect("in a new worktree");
    assert_eq!(worktree.base.as_deref(), Some("develop"), "from the branch picked");
    assert!(worktree.name.starts_with("port-the-parser-"), "{}", worktree.name);
    assert_eq!(start.cwd, "/w/atlas");
}

/// A new worktree's setup on its start tile: while it runs, where it came from and its newest
/// line; once it failed, how, with its last lines and two ways on. "Start without setup" sends
/// the same start again, to the same worktree, without the setup; "Try again" with it.
#[gpui::test]
fn a_start_tile_says_its_worktrees_setup_and_takes_it_again(cx: &mut TestAppContext) {
    use slopty_proto::thread::IntentId;
    use slopty_proto::thread::wire::Setup;

    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    let session = SessionId::new();
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::Terminal { session },
        name: None,
        facts: BTreeMap::new(),
    };
    let summary =
        SessionSummary { repo: Some("/w/atlas".to_owned()), ..summary(session, Some("/w/atlas")) };
    let (key, by) = (studio.key, studio.me);
    view.update_in(cx, |v, _window, cx| {
        v.session_opened(key, summary, cx);
        v.apply_sync(key, ItemSync::Delta { version: 2, by, op: ItemOp::Add(item) }, cx);
    });
    cx.run_until_parked();
    studio.drain();
    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_input("new worktree");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let tile = view.read_with(cx, |v, _| v.focused()).expect("the start's tile").item;
    cx.simulate_input("try the other layout");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let starts = |studio: &mut Fake| -> Vec<(IntentId, Start)> {
        studio
            .drain()
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Thread(ThreadRequest::Start { id, start }) => Some((id, *start)),
                _ => None,
            })
            .collect()
    };
    let sent = starts(&mut studio);
    let [(first, start)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    let name = start.worktree.as_ref().expect("a worktree").name.clone();
    let at = |what: &str| format!("{what}-{}", tile.as_uuid());
    let shows = |cx: &mut VisualTestContext, what: &str| {
        cx.debug_bounds(Box::leak(at(what).into_boxed_str())).is_some()
    };

    let setup = |tail: &[&str]| Setup {
        from: "conductor.json".to_owned(),
        tail: tail.iter().map(|l| (*l).to_owned()).collect(),
    };
    for lines in [&["npm install"][..], &["npm install", "added 312 packages"]] {
        let said = setup(lines);
        view.update_in(cx, |v, _w, cx| v.thread_setting_up(key, *first, said, cx));
        settle(cx);
    }
    cx.update(|window, _| window.set_a11y_active(true));
    settle(cx);
    assert!(shows(cx, "setting-up"), "where the setup came from");
    assert!(shows(cx, "setup-line"), "and its newest line");
    let tree = cx.update(|window, _| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Status", Some("Setting up from conductor.json"))));
    let words: Vec<String> = tree.iter().filter_map(|n| n.label.clone()).collect();
    assert!(words.iter().any(|w| w == "added 312 packages"), "the newest: {words:?}");
    assert!(!words.iter().any(|w| w == "npm install"), "one line, the newest only");

    let failed = IntentDone {
        id: *first,
        outcome: Outcome::SetupFailed { setup: setup(&["npm ERR! missing script"]), code: Some(1) },
    };
    view.update_in(cx, |v, _w, cx| v.thread_done(key, &failed, cx));
    settle(cx);
    assert!(!shows(cx, "setting-up"), "it ran");
    assert!(shows(cx, "setup-failed"), "it failed, said on the tile");
    assert!(shows(cx, "setup-tail"), "with its last lines");
    let tree = cx.update(|window, _| crate::a11y::tree(window));
    assert!(
        tree.iter().any(|n| n.is("Group", Some("Setup from conductor.json failed, exit 1"))),
        "how"
    );
    assert!(cx.debug_bounds("thread-composer").is_some(), "the draft given back");
    let press = |cx: &mut VisualTestContext, what: &'static str| {
        let button = cx.debug_bounds(what).expect(what);
        cx.simulate_click(button.center(), Modifiers::none());
        settle(cx);
    };

    press(cx, "setup-skip");
    let sent = starts(&mut studio);
    let [(skip, again)] = sent.as_slice() else { panic!("sent again: {sent:?}") };
    assert_ne!(skip, first, "under a new intent");
    let worktree = again.worktree.as_ref().expect("a worktree");
    assert_eq!((worktree.name.as_str(), worktree.setup), (name.as_str(), false));
    assert_eq!(again.prompt.as_deref(), Some("try the other layout"), "the same start");
    assert!(!shows(cx, "setup-failed"), "starting again");

    let failed = IntentDone {
        id: *skip,
        outcome: Outcome::SetupFailed { setup: setup(&["boom"]), code: None },
    };
    view.update_in(cx, |v, _w, cx| v.thread_done(key, &failed, cx));
    settle(cx);
    press(cx, "setup-again");
    let sent = starts(&mut studio);
    let [(_, again)] = sent.as_slice() else { panic!("sent again: {sent:?}") };
    let worktree = again.worktree.as_ref().expect("a worktree");
    assert_eq!((worktree.name.as_str(), worktree.setup), (name.as_str(), true), "set up again");
}

/// A start in the folder itself offers no other agents: two would share one tree.
#[gpui::test]
fn a_start_in_the_folder_runs_on_one_agent(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    studio.drain();
    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    let at = cx.debug_bounds("thread-attach").expect("the + button");
    cx.simulate_click(at.center(), Modifiers::none());
    settle(cx);
    assert!(cx.debug_bounds("thread-add-menu-files").is_some(), "the menu is open");
    assert!(cx.debug_bounds("thread-add-menu-also-claude-code").is_none(), "no other agent");
}

/// The folder step takes a folder typed from its root as a line of its own, and starts there.
/// A thread's own tile offers a new worktree of the repository its agent works in, as its
/// worker's table says, with no shell standing there.
#[gpui::test]
fn the_folder_step_takes_a_typed_folder_and_a_threads_repository(cx: &mut TestAppContext) {
    use super::super::agent_start::{NEW_WORKTREE, TYPED_FOLDER};

    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    studio.drain();
    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_input("/srv/api/");
    settle(cx);
    let typed = format!("{TYPED_FOLDER} /srv/api");
    assert!(step_lines(&view, cx).contains(&typed), "{:?}", step_lines(&view, cx));
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_input("go");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent = starts(&mut studio);
    let [(_, _, cwd, _)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!(cwd, "/srv/api", "started where it was typed");

    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.agent = AgentId::named(AgentId::CODEX);
    state.meta.cwd = "/w/atlas".to_owned();
    let thread = state.meta.id;
    let mut row = state.row(WallMs::ZERO);
    row.repo = Some("/w/atlas".to_owned());
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        let table = slopty_proto::thread::wire::TableFrame::Snapshot {
            cursor: slopty_proto::thread::Cursor { epoch: 1, seq: 1 },
            rows: vec![row],
        };
        v.thread_table(key, &table, cx);
        v.open_thread(key, thread, cx);
    });
    settle(cx);
    let tile = view.read_with(cx, |v, _| v.tile_of_thread(thread)).expect("the thread's tile");
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    settle(cx);
    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let lines = step_lines(&view, cx);
    assert_eq!(lines.first().map(String::as_str), Some("w/atlas"), "its folder first: {lines:?}");
    assert!(lines.contains(&format!("{NEW_WORKTREE} atlas")), "{lines:?}");
}

/// Every start lists the places work stood, newest first, with no shell open: a thread's
/// folder, and the folders the machine lists of the agent's past sessions, as its link comes up
/// and again as the folder step opens, joining the step that is up. The empty workspace offers
/// the same places, and one pressed starts the machine's usual agent there.
#[gpui::test]
fn every_start_offers_where_threads_and_past_sessions_worked(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::{PastSession, PastSessions};

    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, worker_key(WorkerId::new()).value(), "studio");
    let claude = AgentId::named(AgentId::CLAUDE_CODE);
    let key = studio.key;
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.agent = claude.clone();
    state.meta.cwd = "/w/atlas".to_owned();
    let row = state.row(WallMs::from_millis(2_000));
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(key, with_agents(std::slice::from_ref(&claude)), cx);
        v.threads_linked(key, cx);
        let table = slopty_proto::thread::wire::TableFrame::Snapshot {
            cursor: slopty_proto::thread::Cursor { epoch: 1, seq: 1 },
            rows: vec![row],
        };
        v.thread_table(key, &table, cx);
    });
    settle(cx);
    let asks =
        |fake: &mut Fake| -> Vec<Option<AgentId>> {
            fake.drain()
                .into_iter()
                .filter_map(|m| match m {
                    ClientMsg::Thread(ThreadRequest::Sessions {
                        agent, cwd: None, query, ..
                    }) if query.is_empty() => Some(agent),
                    _ => None,
                })
                .collect()
        };
    assert_eq!(asks(&mut studio), [None], "the link up asks for every agent's sessions");
    let past = |agent: Option<AgentId>, at: &[(&str, u64)]| PastSessions {
        agent,
        cwd: None,
        query: String::new(),
        sessions: at
            .iter()
            .map(|(cwd, ms)| PastSession {
                agent: claude.clone(),
                native: format!("s-{ms}"),
                cwd: Some((*cwd).to_owned()),
                title: None,
                updated_ms: Some(WallMs::from_millis(*ms)),
                thread: None,
                resume: Vec::new(),
                facts: BTreeMap::new(),
                prompts: Vec::new(),
            })
            .collect(),
        absent: None,
        cut: None,
    };
    view.update_in(cx, |v, _w, cx| v.past_sessions(key, past(None, &[("/w/old", 1_000)]), cx));

    cx.dispatch_action(NewAgent);
    settle(cx);
    assert_eq!(
        step_lines(&view, cx),
        ["w/atlas", "w/old", "~", RESUME_PAST],
        "no shell: the thread's folder, then the past session's, the newest first"
    );
    assert_eq!(asks(&mut studio), [Some(claude.clone())], "the step asks again");
    let newer = past(Some(claude.clone()), &[("/w/old", 1_000), ("/w/new", 3_000)]);
    view.update_in(cx, |v, _w, cx| v.past_sessions(key, newer, cx));
    settle(cx);
    assert_eq!(
        step_lines(&view, cx),
        ["w/new", "w/atlas", "w/old", "~", RESUME_PAST],
        "what the machine lists joins the step that is up"
    );
    cx.simulate_keystrokes("escape");
    settle(cx);

    let places = view.read_with(cx, |v, cx| v.recent_places(None, cx));
    let cwds: Vec<&str> = places.iter().map(|p| p.cwd.as_str()).collect();
    assert_eq!(cwds, ["/w/new", "/w/atlas", "/w/old"], "the empty workspace's places");
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    settle(cx);
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let says =
        |label: &str| tree.iter().any(|n| n.role == "Button" && n.label.as_deref() == Some(label));
    let labels: Vec<_> =
        tree.iter().filter(|n| n.role == "Button").map(|n| n.label.clone()).collect();
    assert!(says("New Claude Code agent in w/atlas"), "a place says what it starts: {labels:?}");
    let row = cx.debug_bounds("empty-place-1").expect("the second place is drawn");
    cx.simulate_click(row.center(), Modifiers::default());
    settle(cx);
    cx.simulate_input("carry on");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent = starts(&mut studio);
    let [(_, agent, cwd, _)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!((agent, cwd.as_str()), (&claude, "/w/atlas"), "the usual agent, there");
}

/// The folder step and "Resume a past session…" list the same thing, so one ask goes for both:
/// while the folder step's is out the session step sends none, and its answer feeds both, the
/// session step's lines and the places. Once it is answered, the next step asks again.
#[gpui::test]
fn one_ask_for_past_sessions_feeds_the_folder_and_session_steps(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::{PastSession, PastSessions};

    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    let codex = AgentId::named(AgentId::CODEX);
    let key = studio.key;
    let asks = |fake: &mut Fake| {
        fake.drain()
            .into_iter()
            .filter(|m| {
                matches!(m, ClientMsg::Thread(ThreadRequest::Sessions { cwd: None, query, .. })
                    if query.is_empty())
            })
            .count()
    };
    studio.drain();
    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(asks(&mut studio), 1, "the folder step asks");
    cx.simulate_input("resume");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(asks(&mut studio), 0, "the session step waits on the same ask");

    let answer = PastSessions {
        agent: Some(codex.clone()),
        cwd: None,
        query: String::new(),
        sessions: vec![PastSession {
            agent: codex.clone(),
            native: "019c".to_owned(),
            cwd: Some("/w/late".to_owned()),
            title: Some("Port the parser".to_owned()),
            updated_ms: Some(WallMs::from_millis(5_000)),
            thread: None,
            resume: Vec::new(),
            facts: BTreeMap::new(),
            prompts: Vec::new(),
        }],
        absent: None,
        cut: None,
    };
    view.update_in(cx, |v, _w, cx| v.past_sessions(key, answer, cx));
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["Port the parser"], "the session step lists it");
    let places = view.read_with(cx, |v, cx| v.recent_places(Some(&codex), cx));
    assert!(places.iter().any(|p| p.cwd == "/w/late"), "and the places have its folder");

    cx.simulate_keystrokes("escape");
    settle(cx);
    cx.dispatch_action(NewAgent);
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(asks(&mut studio), 1, "answered, the next step asks again");
}

/// A relaunch begins where the last run's start left off: the agent step lists its agent
/// first, the folder step its new worktree first, and the draft's chips begin on that agent's
/// last choices the machine still offers. The start that goes is what the next run reads.
#[gpui::test]
fn a_relaunch_starts_where_the_last_run_left_off(cx: &mut TestAppContext) {
    use slopty_client::starts::{Chips, LastStart, Starts};
    use slopty_proto::thread::{Effort, Mode, Model, Offers};

    use super::super::agent_start::NEW_WORKTREE;

    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join(slopty_client::starts::FILE);
    let key = worker_key(WorkerId::new());
    let codex = AgentId::named(AgentId::CODEX);
    let mut kept = Starts::default();
    let chips = |model: &str, mode: &str, effort: Option<&str>| Chips {
        model: Some(model.to_owned()),
        mode: Some(mode.to_owned()),
        effort: effort.map(str::to_owned),
    };
    let last = LastStart {
        agent: codex.clone(),
        worker: key,
        cwd: "/w/atlas".to_owned(),
        worktree: true,
        at: WallMs::from_millis(1_000),
    };
    kept.went(last, Some(chips("gpt-5", "auto", Some("xhigh"))));
    kept.write(&path).expect("the last run's starts");

    let (view, cx) = still_workspace(cx);
    view.update_in(cx, |v, _w, _cx| v.set_starts_file(path.clone()));
    let mut studio = connect(&view, cx, key.value(), "studio");
    let choice = |id: &str| (id.to_owned(), id.to_uppercase());
    let offers = Offers {
        models: ["gpt-5", "gpt-4"].map(|id| Model { id: choice(id).0, label: choice(id).1 }).into(),
        modes: vec![Mode { id: "auto".to_owned(), label: "Auto".to_owned(), description: None }],
        mode: None,
        efforts: vec![Effort {
            id: "high".to_owned(),
            label: "High".to_owned(),
            description: None,
        }],
        commands: Vec::new(),
    };
    let installed = |agent: &AgentId, offers: Offers| slopty_proto::server::InstalledAgent {
        agent: agent.clone(),
        version: "1.0".to_owned(),
        offers,
    };
    let claude = AgentId::named(AgentId::CLAUDE_CODE);
    let caps = WorkerCaps {
        agents: vec![installed(&claude, Offers::default()), installed(&codex, offers)],
        ..healthy()
    };
    let session = SessionId::new();
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::Terminal { session },
        name: None,
        facts: BTreeMap::new(),
    };
    let summary =
        SessionSummary { repo: Some("/w/atlas".to_owned()), ..summary(session, Some("/w/atlas")) };
    let by = studio.me;
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(key, caps, cx);
        v.session_opened(key, summary, cx);
        v.apply_sync(key, ItemSync::Delta { version: 2, by, op: ItemOp::Add(item) }, cx);
    });
    settle(cx);
    studio.drain();

    cx.dispatch_action(NewAgent);
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["Codex", "Claude Code"], "the last run's agent first");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let lines = step_lines(&view, cx);
    assert_eq!(
        lines.first().cloned(),
        Some(format!("{NEW_WORKTREE} atlas")),
        "its new worktree first: {lines:?}"
    );
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_input("go on");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent: Vec<Start> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some(*start),
            _ => None,
        })
        .collect();
    let [start] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!((&start.agent, start.cwd.as_str()), (&codex, "/w/atlas"));
    assert!(start.worktree.is_some(), "in a new worktree again");
    assert_eq!(
        (start.model.as_deref(), start.mode.as_deref(), start.effort.as_deref()),
        (Some("gpt-5"), Some("auto"), None),
        "the chips begin on the last choices offered; an effort no longer offered is the default"
    );

    let next = Starts::read(&path);
    assert_eq!(next.last().map(|l| (l.worktree, l.cwd.as_str())), Some((true, "/w/atlas")));
    assert_eq!(next.chips(&codex), Some(&chips("gpt-5", "auto", None)), "what went, kept");
}

/// A folder's changes tile on `/w/atlas`, on a studio with `agents` where a Codex thread last
/// worked in that folder, with one comment written on its added line.
fn a_folder_commented<'a>(
    cx: &'a mut TestAppContext,
    agents: &[AgentId],
) -> (Entity<WorkspaceView>, Fake, &'a mut VisualTestContext) {
    use slopty_core::WallMs;
    use slopty_proto::git::{GitDone, GitOp, GitOutcome};
    use slopty_proto::thread::detail::Hunk;
    use slopty_proto::thread::wire::{Against, FileDiff, Review, ReviewScope, TableFrame};
    use slopty_proto::thread::{Cursor, Patch};

    use super::super::actions::ReviewChanges;

    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let mut ran = crate::conversation::thread::fixtures::thread("edit");
    ran.meta.agent = AgentId::named(AgentId::CODEX);
    "/w/atlas".clone_into(&mut ran.meta.cwd);
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![ran.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(key, with_agents(agents), cx);
        v.threads_linked(key, cx);
        v.thread_table(key, &table, cx);
    });
    let folder = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/atlas".into() }, 1);
    view.update_in(cx, |v, _w, cx| v.focus_tile(folder, cx));
    settle(cx);
    studio.drain();
    cx.dispatch_action(ReviewChanges);
    settle(cx);
    let asked = studio.drain().into_iter().find_map(|m| match m {
        ClientMsg::Git { request, op: GitOp::Changes { .. }, .. } => Some(request),
        _ => None,
    });
    let request = asked.expect("the folder's changes asked");
    let lines = [" fn main() {", "-    old();", "+    new();", " }"];
    let file = FileDiff {
        path: "src/lib.rs".to_owned(),
        from: Some("old".to_owned()),
        to: Some("new".to_owned()),
        binary: false,
        patch: Patch {
            hunks: vec![Hunk {
                old_start: 10,
                old_lines: 3,
                new_start: 10,
                new_lines: 3,
                heading: None,
                lines: lines.map(str::to_owned).to_vec(),
            }],
            added: 1,
            removed: 1,
            clipped_lines: 0,
            full: None,
        },
    };
    let review = Review {
        scope: ReviewScope::WorkingTree(Against::Head),
        from: None,
        to: None,
        files: vec![file],
        absent: None,
    };
    let done = GitOutcome::Done(GitDone::Changes(Box::new(review)));
    view.update_in(cx, |v, _w, cx| v.git_done(key, request, done, cx));
    settle(cx);
    assert!(cx.debug_bounds("review-send-new").is_none(), "no foot with no comment");

    let line = cx.debug_bounds("review-line-0-0-2").expect("the added line").center();
    cx.simulate_click(line, Modifiers::none());
    settle(cx);
    cx.simulate_input("Why new?");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert!(cx.debug_bounds("review-comment-0").is_some(), "a folder's review takes comments");
    (view, studio, cx)
}

/// The comments waiting on the folder's review.
fn comments_waiting(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> usize {
    view.update(cx, |v, cx| v.changes_views().map(|r| r.read(cx).waiting()).sum::<usize>())
}

/// A folder's review takes comments, and its foot sends them to a new agent there: the agent
/// the folder last ran (Codex here, not the machine's first, Claude Code), its start in the
/// folder with the comments quoted in its composer to be added to, and none left on the
/// review. ↵ starts it with them as its first message.
#[gpui::test]
fn a_folders_comments_go_to_a_new_agent_there(cx: &mut TestAppContext) {
    let agents = [AgentId::named(AgentId::CLAUDE_CODE), AgentId::named(AgentId::CODEX)];
    let (view, mut studio, cx) = a_folder_commented(cx, &agents);
    assert!(cx.debug_bounds("review-add").is_none(), "no thread's draft to add to");
    assert!(cx.debug_bounds("review-mark").is_none(), "and nothing to keep for an agent");
    let send = cx.debug_bounds("review-send-new").expect("Send to a new agent").center();
    cx.simulate_click(send, Modifiers::none());
    settle(cx);

    let quoted = "In `src/lib.rs` line 11:\n```diff\n+    new();\n```\nWhy new?";
    let start = view.read_with(cx, |v, _| v.focused()).expect("the start's tile");
    let drafted =
        view.update(cx, |v, cx| v.starting.draft_view(start.item).map(|d| d.read(cx).draft(cx)));
    assert_eq!(drafted.as_deref(), Some(quoted), "the comments in the start's composer");
    assert_eq!(comments_waiting(&view, cx), 0, "none left once the composer took them");

    cx.simulate_keystrokes("enter");
    settle(cx);
    let starts: Vec<Start> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some(*start),
            _ => None,
        })
        .collect();
    let [start] = starts.as_slice() else { panic!("one start: {starts:?}") };
    assert_eq!(start.agent, AgentId::named(AgentId::CODEX), "the agent the folder last ran");
    assert_eq!(start.cwd, "/w/atlas");
    assert_eq!(start.prompt.as_deref(), Some(quoted));
    assert!(start.worktree.is_none(), "in the folder itself");
}

/// With no agent the machine can start, the send opens nothing, says why, and keeps the
/// comments on the review to send once there is one.
#[gpui::test]
fn a_folders_comments_stay_when_no_agent_can_take_them(cx: &mut TestAppContext) {
    let (view, mut studio, cx) = a_folder_commented(cx, &[]);
    let review = view.read_with(cx, |v, _| v.focused()).expect("the review's tile");
    let send = cx.debug_bounds("review-send-new").expect("Send to a new agent").center();
    cx.simulate_click(send, Modifiers::none());
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.focused()), Some(review), "no start opened");
    assert!(cx.debug_bounds("review-comment-0").is_some(), "the comment stays");
    assert_eq!(comments_waiting(&view, cx), 1);
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()).as_deref(), Some(agent_start::NO_AGENT));
    let started = studio
        .drain()
        .into_iter()
        .any(|m| matches!(m, ClientMsg::Thread(ThreadRequest::Start { .. })));
    assert!(!started, "nothing started");
}

/// "Review a pull request…": which repository, each clone once (a thread in one of its
/// worktrees names the clone), then which pull request, by its number or its page. The
/// machine's agent starts in a new worktree that checks it out, named for it, its composer
/// asking for the review, and once the thread is there its review opens beside it on the
/// whole branch, against the branch the pull request merges into once the worker says which.
#[gpui::test]
fn a_pull_request_is_reviewed_in_a_worktree_that_checks_it_out(cx: &mut TestAppContext) {
    use slopty_proto::thread::Cursor;
    use slopty_proto::thread::wire::{
        Against, PullSeen, PullStands, ReviewScope, TableFrame, ThreadFrame,
    };

    use super::super::actions::ReviewPull;
    use super::super::pull_review::pull_number;

    assert_eq!(pull_number(" #123 "), Some(123));
    assert_eq!(pull_number("123"), Some(123));
    assert_eq!(pull_number("https://github.com/o/atlas/pull/123/files"), Some(123));
    assert_eq!(pull_number("https://github.com/o/atlas/pull/9#discussion"), Some(9));
    for not in ["", "#", "0", "#-1", "fix 12", "https://github.com/o/atlas/issues/4"] {
        assert_eq!(pull_number(not), None, "{not:?}");
    }

    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    let key = studio.key;
    for (version, cwd, repo) in [
        (2, "/w/atlas", "/w/atlas"),
        (3, "/w/atlas/.claude/worktrees/fix-1a2b", "/w/atlas/.claude/worktrees/fix-1a2b"),
        (4, "/w/blog", "/w/blog"),
    ] {
        let session = SessionId::new();
        let item = Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            name: None,
            facts: BTreeMap::new(),
        };
        let summary = SessionSummary { repo: Some(repo.to_owned()), ..summary(session, Some(cwd)) };
        let by = studio.me;
        view.update_in(cx, |v, _window, cx| {
            v.session_opened(key, summary, cx);
            v.apply_sync(key, ItemSync::Delta { version, by, op: ItemOp::Add(item) }, cx);
        });
    }
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    cx.run_until_parked();
    studio.drain();

    view.update_in(cx, |v, window, cx| v.review_pull(&ReviewPull, window, cx));
    settle(cx);
    let mut lines = step_lines(&view, cx);
    lines.sort();
    assert_eq!(lines, ["atlas", "blog"], "each clone once, on the one machine with them");
    cx.simulate_input("atlas");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert!(step_lines(&view, cx).is_empty(), "nothing until a number");
    cx.simulate_input("https://github.com/o/atlas/pull/123/files");
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["Review #123 in atlas"]);
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.update(|window, _| window.set_a11y_active(true));
    settle(cx);
    let tree = cx.update(|window, _| crate::a11y::tree(window));
    let asked = "What should Claude Code do with #123 in a new worktree of atlas?";
    let heads: Vec<_> =
        tree.iter().filter(|n| n.role == "Heading").map(|n| n.label.clone()).collect();
    assert!(tree.iter().any(|n| n.is("Heading", Some(asked))), "which, asked: {heads:?}");

    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent: Vec<(slopty_proto::thread::IntentId, Start)> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { id, start }) => Some((id, *start)),
            _ => None,
        })
        .collect();
    let [(intent, start)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!(start.prompt.as_deref(), Some("Review pull request #123"), "the composer's ask");
    assert_eq!(start.cwd, "/w/atlas", "from the clone");
    let worktree = start.worktree.as_ref().expect("a worktree");
    assert_eq!(worktree.pull, Some(123), "checking the pull request out");
    let suffix = worktree.name.strip_prefix("pr-123-").expect("named for it");
    assert!(suffix.len() == 4 && suffix.chars().all(|c| c.is_ascii_hexdigit()), "{suffix}");

    let mut state = crate::conversation::thread::fixtures::thread("edit");
    let thread = state.meta.id;
    state.meta.cwd = format!("/w/atlas/.claude/worktrees/{}", worktree.name);
    let started = IntentDone { id: *intent, outcome: Outcome::Started { thread } };
    view.update_in(cx, |v, _w, cx| {
        let table = TableFrame::Snapshot {
            cursor: Cursor { epoch: 1, seq: 1 },
            rows: vec![state.row(WallMs::ZERO)],
        };
        v.thread_table(key, &table, cx);
        v.thread_done(key, &started, cx);
    });
    settle(cx);
    let reviewed = studio.drain().into_iter().any(|m| {
        matches!(m, ClientMsg::Items(ItemOp::Add(Item { kind: ItemKind::Review { thread: t }, .. }))
            if t == thread)
    });
    assert!(reviewed, "its review opens beside it");
    let scope = view.read_with(cx, |v, cx| v.review_of(thread).map(|r| r.read(cx).scope()));
    assert_eq!(scope, Some(crate::review::Scope::WholeBranch), "on the whole branch");

    let asks = |studio: &mut Fake| -> Vec<ReviewScope> {
        studio
            .drain()
            .into_iter()
            .filter_map(|m| match m {
                // The thread's own card of its last turn's changes asks its turn; the review
                // tile's asks are the rest.
                ClientMsg::Thread(ThreadRequest::Review { thread: t, scope })
                    if t == thread && !matches!(scope, ReviewScope::Turn(_)) =>
                {
                    Some(scope)
                }
                _ => None,
            })
            .collect()
    };
    let frame = |pull: Option<PullSeen>, seq: u64| {
        let mut state = state.clone();
        state.pull = pull;
        ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq }, state: Box::new(state) }
    };
    view.update_in(cx, |v, _w, cx| v.thread_frame(key, thread, frame(None, 1), cx));
    settle(cx);
    assert_eq!(asks(&mut studio), [ReviewScope::WorkingTree(Against::Base)], "a guessed base");
    let pull = PullSeen {
        forge: slopty_proto::git::Forge::GitHub,
        number: 123,
        url: "https://github.com/o/atlas/pull/123".to_owned(),
        title: "Fix the build".to_owned(),
        base: "release".to_owned(),
        stands: PullStands::Waiting,
        failed: 0,
        failed_first: None,
        running: 0,
    };
    view.update_in(cx, |v, _w, cx| v.thread_frame(key, thread, frame(Some(pull), 2), cx));
    settle(cx);
    let against = ReviewScope::WorkingTree(Against::Branch("release".to_owned()));
    assert_eq!(asks(&mut studio), [against], "the branch it merges into, as the forge says");
}

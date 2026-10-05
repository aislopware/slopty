//! "New agent…" (⌘⇧T), the one start: the agent, then the machine, then the folder, each step
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

/// ⌘⇧T asks which agent, then (where several can start it) which machine, then which folder,
/// the focused shell's first; ↩ on each starts it. The next ⌘⇧T lists that agent, machine and
/// folder first, so ↩ ↩ starts the same again.
#[gpui::test]
fn new_agent_opens_the_picker_with_the_last_choices(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    studio.drain();

    cx.simulate_keystrokes("cmd-shift-t");
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["Claude Code", "Codex"], "which agent");
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

    cx.simulate_keystrokes("cmd-shift-t");
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
/// ⌘⇧T passes the agent step and asks which machine. A machine whose link found nothing offers
/// nothing, and with none at all ⌘⇧T says so.
#[gpui::test]
fn a_start_with_no_server_lists_each_machines_agents(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _laptop = connect(&view, cx, 2, "laptop");
    let shell = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/src/app"));
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    assert_eq!(agent_labels(&view, cx), ["New Claude Code agent"]);
    cx.simulate_keystrokes("cmd-shift-t");
    settle(cx);
    assert_eq!(step_lines(&view, cx), ["studio", "laptop"], "one agent: which machine");
    cx.simulate_keystrokes("escape");
    settle(cx);
}

/// A machine whose link found no agent offers none, and with none anywhere ⌘⇧T says so.
#[gpui::test]
fn with_no_agent_anywhere_new_agent_says_so(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let caps = WorkerCaps { agents: Vec::new(), ..healthy() };
    view.update_in(cx, |v, _w, cx| v.set_worker_caps(studio.key, caps, cx));
    cx.run_until_parked();
    assert_eq!(agent_labels(&view, cx), Vec::<String>::new());
    cx.simulate_keystrokes("cmd-shift-t");
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
    caps.agents
        .push(InstalledAgent { agent: AgentId::named(AgentId::CODEX), version: "0.50".into() });
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

    cx.simulate_keystrokes("cmd-shift-t");
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
    assert_eq!(asked, [(Some(codex.clone()), None, String::new())], "the machine is asked");
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

/// "Plan first" under a Claude Code start's field starts it in plan mode, its published
/// `--permission-mode plan`, by a click or the palette's "Start in plan mode"; a Codex start
/// has no such tick.
#[gpui::test]
fn a_claude_code_start_can_plan_first(cx: &mut TestAppContext) {
    use super::super::starting::{PLAN_FIRST_LINE, plan_first_selector};

    let (view, cx) = still_workspace(cx);
    let Two { mut studio, .. } = two_machines(&view, cx);
    studio.drain();
    let args = |studio: &mut Fake| -> Vec<Vec<String>> {
        studio
            .drain()
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some(start.args),
                _ => None,
            })
            .collect()
    };
    // The agent, then (Claude Code is on both machines) the machine, then the folder.
    let begin = |agent: &str, steps: usize, cx: &mut VisualTestContext| {
        cx.simulate_keystrokes("cmd-shift-t");
        settle(cx);
        cx.simulate_input(agent);
        for _ in 0..steps {
            cx.simulate_keystrokes("enter");
            settle(cx);
        }
        let asking = |v: &WorkspaceView| {
            v.layout().tiles().find(|t| v.starting.get(t.item).is_some_and(|s| !s.sent))
        };
        view.read_with(cx, |v, _| asking(v)).expect("its tile, asking")
    };

    let codex = begin("codex", 2, cx);
    assert!(cx.debug_bounds(plan_first_selector(codex.item)).is_none(), "Codex: no tick");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(args(&mut studio), [Vec::<String>::new()]);

    let claude = begin("claude", 3, cx);
    let tick = cx.debug_bounds(plan_first_selector(claude.item)).expect("the tick");
    cx.simulate_click(tick.center(), Modifiers::none());
    settle(cx);
    let offered = view.update(cx, |v, cx| v.palette_lines(cx));
    assert!(offered.iter().any(|l| l.label == PLAN_FIRST_LINE), "and in the palette");
    cx.simulate_input("make a plan");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(args(&mut studio), [vec!["--permission-mode".to_owned(), "plan".to_owned()]]);
}

/// The folder step offers a new worktree of each repository its folders are in, once each,
/// after the folders; picking it starts the agent in that folder with a worktree of its own,
/// named after the agent, which the machine makes from the folder's clone.
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

    cx.simulate_keystrokes("cmd-shift-t");
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
    let name = start.worktree.as_deref().expect("a worktree");
    let suffix = name.strip_prefix("codex-").expect("named after its agent");
    assert!(suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_hexdigit()), "{name}");
    assert_eq!(start.cwd, "/w/atlas", "the first folder in it, the most recent shell's");
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
    cx.simulate_keystrokes("cmd-shift-t");
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
    cx.simulate_keystrokes("cmd-shift-t");
    settle(cx);
    cx.simulate_input("codex");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let lines = step_lines(&view, cx);
    assert_eq!(lines.first().map(String::as_str), Some("w/atlas"), "its folder first: {lines:?}");
    assert!(lines.contains(&format!("{NEW_WORKTREE} atlas")), "{lines:?}");
}

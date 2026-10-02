//! An agent's thread started from the palette: a "New … thread" line for each agent the
//! worker's facts say it can start, the start sent to that worker, and the thread it answers
//! with opened as a tile of its own.

use std::collections::BTreeMap;

use slopty_core::WorkerId;
use slopty_proto::orchestration::{Outcome as Answer, Verb};
use slopty_proto::project::{Fact, Facts, WorkerFacts};
use slopty_proto::thread::wire::{IntentDone, Outcome, ThreadRequest};
use slopty_proto::thread::{AgentId, ThreadId};

use super::super::projects::worker_key;
use super::*;
use crate::palette::PaletteRun;

/// What a worker reports of itself: the programs `agents` found (with their versions) and
/// the ACP agents its registry lists.
fn facts_of(worker: WorkerId, agents: &[&str], acp: &[&str]) -> WorkerFacts {
    let map = |names: &[&str]| {
        Fact::Map(names.iter().map(|n| ((*n).to_owned(), Fact::Text("1.0".into()))).collect())
    };
    let facts: Facts = BTreeMap::from([("agents".into(), map(agents)), ("acp".into(), map(acp))]);
    WorkerFacts { worker, facts }
}

/// The palette's "New … thread" labels, in order.
fn start_labels(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.update(cx, |v, cx| v.palette_lines(cx))
        .into_iter()
        .filter(|l| {
            matches!(&l.run, PaletteRun::Action(a)
                if a.as_any().downcast_ref::<StartThread>().is_some())
        })
        .map(|l| l.label)
        .collect()
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

/// The palette offers exactly the agents the focused tile's worker can start: Claude Code,
/// Codex and pi as its facts found them, then each ACP agent, and nothing for a program with
/// no thread (aider) or another worker's agents. With no server it offers none. A line sends
/// the start to that worker with no prompt; the thread it answers with opens as a thread tile
/// that takes the keyboard, and a second answer for it goes to that tile. A refusal is said
/// in the worker's words and opens nothing.
#[gpui::test]
fn the_palette_starts_each_agent_the_worker_offers(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let (studio_id, laptop_id) = (WorkerId::new(), WorkerId::new());
    let mut studio = connect(&view, cx, worker_key(studio_id).value(), "studio");
    let laptop = connect(&view, cx, worker_key(laptop_id).value(), "laptop");
    let laptop_shell = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 1);
    let shell = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/src/app"));
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    assert_eq!(start_labels(&view, cx), Vec::<String>::new(), "no server, no agents known");

    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let (studio_key, laptop_key) = (studio.key, laptop.key);
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.threads_linked(studio_key, cx);
        v.threads_linked(laptop_key, cx);
    });
    cx.run_until_parked();
    let facts = vec![
        facts_of(studio_id, &["aider", "claude", "codex"], &["gemini"]),
        facts_of(laptop_id, &["pi"], &[]),
    ];
    let mut asked = Vec::new();
    while let Some((verb, reply)) = queue.try_next() {
        let _gone = reply.send(Answer::Facts(facts.clone()));
        asked.push(verb);
    }
    assert_eq!(asked, [Verb::WorkerFacts { worker: None }], "one question while one is out");
    cx.run_until_parked();
    assert_eq!(
        start_labels(&view, cx),
        ["New Claude Code thread", "New Codex thread", "New gemini thread"],
        "the studio's agents, for the studio's tile"
    );
    let dirs: Vec<Option<String>> = view
        .update(cx, |v, cx| v.palette_lines(cx))
        .into_iter()
        .filter(|l| l.label.starts_with("New ") && l.label.ends_with(" thread"))
        .map(|l| l.cwd)
        .collect();
    assert!(
        dirs.iter().all(|d| d.as_deref() == Some("src/app")),
        "the folder said short, as the navigator says it: {dirs:?}"
    );
    studio.drain();

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("n e w space c o d e x space t h r e a d enter");
    settle(cx);
    let sent = starts(&mut studio);
    let [(intent, agent, cwd, prompt)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!(agent, &AgentId::named(AgentId::CODEX));
    assert_eq!(cwd, "/src/app", "in the folder the focused shell stands in");
    assert_eq!(prompt, &None, "a start carries no prompt");

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

    // A start the worker refuses opens nothing and says why.
    view.update_in(cx, |v, _w, cx| {
        v.start_thread(studio_key, AgentId::named(AgentId::CLAUDE_CODE), "~".into(), cx);
    });
    let sent = starts(&mut studio);
    let [(intent, ..)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    let refused = IntentDone {
        id: *intent,
        outcome: Outcome::Refused { reason: "claude is not on this worker's PATH".into() },
    };
    view.update_in(cx, |v, _w, cx| v.thread_done(studio_key, &refused, cx));
    settle(cx);
    assert!(
        !studio.drain().iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Add(_)))),
        "a refusal adds no tile"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("claude is not on this worker's PATH")
    );

    view.update_in(cx, |v, _w, cx| v.focus_tile(laptop_shell, cx));
    cx.run_until_parked();
    assert_eq!(start_labels(&view, cx), ["New pi thread"], "the laptop's, for the laptop's tile");
}

/// A palette opened before the worker's agents are known takes them as they arrive: what was
/// typed finds the new line, and ↩ starts it, with no closing and opening it again.
#[gpui::test]
fn an_open_palette_takes_the_agents_as_they_arrive(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio_id = WorkerId::new();
    let mut studio = connect(&view, cx, worker_key(studio_id).value(), "studio");
    let shell = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/src/app"));
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let studio_key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.threads_linked(studio_key, cx);
    });
    cx.run_until_parked();
    let dirs: Vec<Option<String>> = view
        .update(cx, |v, cx| v.palette_lines(cx))
        .into_iter()
        .filter(|l| l.label.starts_with("New ") && l.label.ends_with(" thread"))
        .map(|l| l.cwd)
        .collect();
    assert!(
        dirs.iter().all(|d| d.as_deref() == Some("src/app")),
        "the folder said short, as the navigator says it: {dirs:?}"
    );
    studio.drain();

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("n e w space c o d e x space t h r e a d");
    cx.run_until_parked();
    let facts = vec![facts_of(studio_id, &["codex"], &[])];
    while let Some((_verb, reply)) = queue.try_next() {
        let _gone = reply.send(Answer::Facts(facts.clone()));
    }
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent = starts(&mut studio);
    let [(_, agent, ..)] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!(agent, &AgentId::named(AgentId::CODEX), "the line that arrived, chosen");
}

//! "Resume a past session…" asks every machine up that has the agent, not only the step's.

use std::collections::BTreeMap;

use slopty_core::WorkerId;
use slopty_proto::thread::AgentId;
use slopty_proto::thread::wire::{PastSession, PastSessions, PromptHit, ThreadRequest};

use super::super::actions::ResumePastSession;
use super::super::projects::worker_key;
use super::*;
use crate::conversation::thread::find::ASK_AFTER;

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// A machine's capabilities with Claude Code installed.
fn with_claude() -> WorkerCaps {
    let installed = slopty_proto::server::InstalledAgent {
        agent: AgentId::named(AgentId::CLAUDE_CODE),
        version: "2.1.295 (Claude Code)".to_owned(),
        offers: slopty_proto::thread::Offers::default(),
    };
    WorkerCaps { agents: vec![installed], ..healthy() }
}

fn session(native: &str, cwd: &str, prompt: &str) -> PastSession {
    PastSession {
        agent: AgentId::named(AgentId::CLAUDE_CODE),
        native: native.to_owned(),
        cwd: Some(cwd.to_owned()),
        title: None,
        updated_ms: None,
        thread: None,
        resume: vec!["--resume".to_owned(), native.to_owned()],
        facts: BTreeMap::new(),
        prompts: vec![PromptHit {
            text: prompt.to_owned(),
            spans: Vec::new(),
            cut_before: false,
            cut_after: false,
            at_ms: None,
        }],
    }
}

fn answer(query: &str, sessions: Vec<PastSession>) -> PastSessions {
    PastSessions {
        agent: Some(AgentId::named(AgentId::CLAUDE_CODE)),
        cwd: None,
        query: query.to_owned(),
        sessions,
        absent: None,
        cut: None,
    }
}

/// What `fake` was asked for, by words.
fn asked(fake: &mut Fake) -> Vec<String> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Sessions { query, .. }) => Some(query),
            _ => None,
        })
        .collect()
}

/// The step's lines, each with where it ran.
fn lines(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Vec<(String, Option<String>)> {
    view.read_with(cx, |v, cx| {
        v.palette.clone().map(|p| {
            p.read(cx).matches().iter().map(|l| (l.label.clone(), l.cwd.clone())).collect()
        })
    })
    .unwrap_or_default()
}

/// The session step opened on the studio asks the laptop too, which has the same agent, and
/// a machine without it is not asked. The studio's sessions lead, the laptop's say where they
/// are, and once the field rests both are asked for its words: a session found on the laptop
/// only is listed, and picked, it starts there.
#[gpui::test]
fn a_past_session_is_found_on_every_machine(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, worker_key(WorkerId::new()).value(), "studio");
    let mut laptop = connect(&view, cx, worker_key(WorkerId::new()).value(), "laptop");
    let mut bare = connect(&view, cx, worker_key(WorkerId::new()).value(), "bare");
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(studio.key, with_claude(), cx);
        v.set_worker_caps(laptop.key, with_claude(), cx);
        v.set_worker_caps(bare.key, WorkerCaps { agents: Vec::new(), ..healthy() }, cx);
    });
    settle(cx);
    let (..) = (studio.drain(), laptop.drain(), bare.drain());

    let ask = ResumePastSession { worker: studio.key, agent: AgentId::named(AgentId::CLAUDE_CODE) };
    view.update_in(cx, |v, window, cx| v.resume_past_session(&ask, window, cx));
    settle(cx);
    assert_eq!(asked(&mut studio), [""], "the step's machine");
    assert_eq!(asked(&mut laptop), [""], "and the other that has the agent");
    assert_eq!(asked(&mut bare), Vec::<String>::new(), "not one without it");

    view.update_in(cx, |v, _w, cx| {
        v.past_sessions(
            studio.key,
            answer("", vec![session("s1", "/src/app", "fix the lexer")]),
            cx,
        );
    });
    settle(cx);
    assert!(cx.debug_bounds("palette-empty").is_none(), "the studio's lines show at once");
    view.update_in(cx, |v, _w, cx| {
        let listed = vec![session("l1", "/src/web", "style the board")];
        v.past_sessions(laptop.key, answer("", listed), cx);
    });
    settle(cx);
    assert_eq!(
        lines(&view, cx),
        [
            ("fix the lexer".to_owned(), Some("src/app".to_owned())),
            ("style the board".to_owned(), Some("src/web on laptop".to_owned())),
        ],
        "the studio's first; the laptop's say where"
    );

    cx.simulate_input("parser");
    cx.executor().advance_clock(ASK_AFTER);
    settle(cx);
    assert_eq!(asked(&mut studio), ["parser"]);
    assert_eq!(asked(&mut laptop), ["parser"]);
    view.update_in(cx, |v, _w, cx| {
        v.past_sessions(studio.key, answer("parser", Vec::new()), cx);
        let found = vec![session("l0", "/src/api", "the parser drops a token")];
        v.past_sessions(laptop.key, answer("parser", found), cx);
    });
    settle(cx);
    assert_eq!(
        lines(&view, cx),
        [("the parser drops a token".to_owned(), Some("src/api on laptop".to_owned()))],
        "found on the laptop alone"
    );
    cx.simulate_keystrokes("enter");
    settle(cx);
    let started: Vec<_> = laptop
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some((start.cwd, start.args)),
            _ => None,
        })
        .collect();
    let words = vec!["--resume".to_owned(), "l0".to_owned()];
    assert_eq!(started, [("/src/api".to_owned(), words)], "taken up on the laptop");
    assert!(starts_on(&mut studio), "nothing on the studio");
}

/// Whether `fake` was asked to start nothing.
fn starts_on(fake: &mut Fake) -> bool {
    !fake.drain().into_iter().any(|m| matches!(m, ClientMsg::Thread(ThreadRequest::Start { .. })))
}

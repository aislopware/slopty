//! A repository another machine has is offered for cloning in the folder step, and the agent
//! starts in the clone.

use slopty_proto::cloning::{CloneOutcome, ClonedRepo};
use slopty_proto::terminal::RepoId;
use slopty_proto::thread::AgentId;

use super::super::actions::NewAgentOn;
use super::*;

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

fn step_lines(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, cx| {
        v.palette.clone().map(|p| p.read(cx).matches().iter().map(|l| l.label.clone()).collect())
    })
    .unwrap_or_default()
}

/// What the palette up now says while it has no line.
fn step_says(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |v, cx| v.palette.clone().map(|p| p.read(cx).empty_words().to_owned()))
}

/// The clones `fake` was asked for, as (request, url, into).
fn clones_asked(fake: &mut Fake) -> Vec<(u64, String, String)> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::CloneRepo { request, url, into } => Some((request, url, into)),
            _ => None,
        })
        .collect()
}

const SLOPTY: &str = "github.com/aislopware/slopty";
const URL: &str = "https://github.com/aislopware/slopty.git";

/// A laptop whose shell stands in a clone of slopty, and a studio with none: the studio's
/// folder step for Claude Code is up.
fn laptop_has_it(cx: &mut TestAppContext) -> (Entity<WorkspaceView>, &mut VisualTestContext, Fake) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let session = SessionId::new();
    opens_in(&view, cx, &laptop, session, laptop.me, 1, Some("/Users/l/work/slopty"));
    let id = RepoId {
        origin: Some(SLOPTY.to_owned()),
        root: Some("c08d4c1e".to_owned()),
        url: Some(URL.to_owned()),
    };
    let key = laptop.key;
    view.update_in(cx, |v, _w, cx| {
        let summary = SessionSummary {
            repo: Some("/Users/l/work/slopty".to_owned()),
            repo_id: Some(id),
            ..summary(session, Some("/Users/l/work/slopty"))
        };
        v.session_opened(key, summary, cx);
    });
    settle(cx);
    studio.drain();
    let claude = AgentId::named(AgentId::CLAUDE_CODE);
    cx.dispatch_action(NewAgentOn { agent: claude, worker: studio.key });
    settle(cx);
    (view, cx, studio)
}

/// The studio's folder step offers to clone slopty, which only the laptop has; picked, the
/// studio is asked to clone it into the same name under its home, and a step says so at once,
/// then how far git is. Once the clone is there the agent's start opens in it.
#[gpui::test]
fn a_repository_on_another_machine_is_cloned_and_the_agent_starts_in_it(cx: &mut TestAppContext) {
    let (view, cx, mut studio) = laptop_has_it(cx);
    let line = format!("Clone {SLOPTY} into ~/slopty");
    assert!(step_lines(&view, cx).contains(&line), "{:?}", step_lines(&view, cx));
    cx.simulate_input("clone");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let asked = clones_asked(&mut studio);
    let [(request, url, into)] = asked.as_slice() else { panic!("{asked:?}") };
    assert_eq!((url.as_str(), into.as_str()), (URL, "~/slopty"));
    assert_eq!(
        step_says(&view, cx).as_deref(),
        Some(format!("Cloning {SLOPTY} into ~/slopty on studio\u{2026}").as_str()),
        "said at once"
    );
    let (key, request) = (studio.key, *request);
    view.update_in(cx, |v, _w, cx| v.repo_cloning(key, request, "Receiving objects", Some(40), cx));
    assert_eq!(
        step_says(&view, cx).as_deref(),
        Some(
            format!("Cloning {SLOPTY} into ~/slopty on studio \u{b7} Receiving objects 40%")
                .as_str()
        )
    );
    let cloned = ClonedRepo {
        path: "/Users/s/slopty".to_owned(),
        repo: RepoId { origin: Some(SLOPTY.to_owned()), ..RepoId::default() },
    };
    view.update_in(cx, |v, _w, cx| v.repo_cloned(key, request, CloneOutcome::Cloned(cloned), cx));
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.palette.is_none()), "the step is put away");
    let started = view.read_with(cx, |v, _| {
        let tile = v.focused().filter(|t| v.starting.has(t.item))?;
        v.starting.get(tile.item).map(|s| (s.worker, s.cwd.clone()))
    });
    assert_eq!(started, Some((key, "/Users/s/slopty".to_owned())), "the start, in the clone");
}

fn cloned_at() -> ClonedRepo {
    ClonedRepo { path: "/Users/s/slopty".to_owned(), repo: RepoId::default() }
}

/// A refusal is said in the step, in the machine's words, and nothing starts. A link lost with
/// the clone out says so, and its late answer starts nothing. A clone that lands after its step
/// was put away starts nothing and is said as a notice.
#[gpui::test]
fn a_refused_clone_is_said_and_one_whose_step_went_starts_nothing(cx: &mut TestAppContext) {
    let (view, cx, mut studio) = laptop_has_it(cx);
    cx.simulate_input("clone");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let (request, ..) = clones_asked(&mut studio).pop().expect("a clone asked");
    let key = studio.key;
    let why = "~/slopty is there already and is not a clone of github.com/aislopware/slopty";
    let refused = CloneOutcome::Refused { why: why.to_owned() };
    view.update_in(cx, |v, _w, cx| v.repo_cloned(key, request, refused, cx));
    settle(cx);
    assert_eq!(
        step_says(&view, cx),
        Some(format!("Could not clone {SLOPTY} on studio: {why}")),
        "said where the person looks"
    );
    assert!(view.read_with(cx, |v, _| v.focused().is_none_or(|t| !v.starting.has(t.item))));

    cx.simulate_keystrokes("escape");
    settle(cx);
    let claude = AgentId::named(AgentId::CLAUDE_CODE);
    cx.dispatch_action(NewAgentOn { agent: claude, worker: key });
    settle(cx);
    cx.simulate_input("clone");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let (request, ..) = clones_asked(&mut studio).pop().expect("asked again");
    view.update_in(cx, |v, _w, cx| v.disconnect_worker(key, WorkerStatus::Unreachable, cx));
    settle(cx);
    assert_eq!(
        step_says(&view, cx),
        Some(format!("studio went out of reach before {SLOPTY} was cloned")),
        "its answer never comes"
    );
    view.update_in(cx, |v, _w, cx| {
        v.repo_cloned(key, request, CloneOutcome::Cloned(cloned_at()), cx);
    });
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.focused().is_none_or(|t| !v.starting.has(t.item))));
    cx.simulate_keystrokes("escape");
    settle(cx);
    // Linked again.
    let mut studio = connect(&view, cx, 1, "studio");
    let claude = AgentId::named(AgentId::CLAUDE_CODE);
    cx.dispatch_action(NewAgentOn { agent: claude, worker: key });
    settle(cx);
    cx.simulate_input("clone");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let (request, ..) = clones_asked(&mut studio).pop().expect("asked once more");
    cx.simulate_keystrokes("escape");
    settle(cx);
    view.update_in(cx, |v, _w, cx| {
        v.repo_cloned(key, request, CloneOutcome::Cloned(cloned_at()), cx);
    });
    settle(cx);
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some(format!("Cloned {SLOPTY} into /Users/s/slopty on studio").as_str())
    );
    assert!(view.read_with(cx, |v, _| v.focused().is_none_or(|t| !v.starting.has(t.item))));
}

//! "Run": the repository's own run scripts, asked of the machine in the folder the focus works
//! in, one opened at once and several picked from a step.

use slopty_proto::RequestId;
use slopty_proto::git::{GitDone, GitOp, GitOutcome, RunScript, RunScripts};
use slopty_proto::terminal::OpenSession;

use super::*;
use crate::workspace::actions::RunHere;
use crate::workspace::run_scripts::RUN;

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

fn script(name: &str, line: &str) -> RunScript {
    RunScript {
        name: name.to_owned(),
        line: line.to_owned(),
        command: vec!["/bin/zsh".to_owned(), "-lc".to_owned(), line.to_owned()],
        cwd: "/w/app".to_owned(),
        env: vec![("SLOPTY_ROOT_PATH".to_owned(), "/w/app".to_owned())],
    }
}

fn scripts(list: Vec<RunScript>) -> GitOutcome {
    let from = (!list.is_empty()).then(|| ".conductor/settings.toml".to_owned());
    GitOutcome::Done(GitDone::Scripts(Box::new(RunScripts { from, list })))
}

/// The scripts asked of `fake`, as (request, folder).
fn asked(fake: &mut Fake) -> Vec<(RequestId, String)> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Git { request, repo, op: GitOp::Scripts } => Some((request, repo)),
            _ => None,
        })
        .collect()
}

/// The sessions `fake` was asked to open.
fn opened(fake: &mut Fake) -> Vec<OpenSession> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::OpenSession { spec, .. } => Some(spec),
            _ => None,
        })
        .collect()
}

/// The step's lines, as (name, what is muted beside it).
fn step_lines(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Vec<(String, String)> {
    view.read_with(cx, |v, cx| {
        v.palette
            .clone()
            .map(|p| p.read(cx).matches().iter().map(|l| (l.label.clone(), l.context())).collect())
    })
    .unwrap_or_default()
}

fn step_says(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |v, cx| v.palette.clone().map(|p| p.read(cx).empty_words().to_owned()))
}

/// A folder tile in /w/app on a linked studio, focused.
fn in_app(cx: &mut TestAppContext) -> (Entity<WorkspaceView>, &mut VisualTestContext, Fake) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let app = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/app".into() }, 1);
    view.update_in(cx, |v, _w, cx| v.focus_tile(app, cx));
    settle(cx);
    (view, cx, studio)
}

/// Run is offered where the focus works in a folder and asks its machine for the scripts
/// there, the step saying so meanwhile. Several are listed by name with their script muted, the
/// default first; the one picked opens in a terminal with its command, folder, environment and
/// name. A second Run lists the scripts read last at once, and the fresh list that comes takes
/// their place without opening anything unpicked.
#[gpui::test]
fn run_lists_the_repository_scripts_and_opens_the_one_picked(cx: &mut TestAppContext) {
    let (view, cx, mut studio) = in_app(cx);
    let key = studio.key;
    let offered = view.update_in(cx, |v, window, cx| v.offered_lines(window, cx));
    assert!(offered.iter().any(|l| l.label == RUN), "offered in a folder");

    studio.drain();
    cx.dispatch_action(RunHere);
    settle(cx);
    let ask = asked(&mut studio);
    let [(request, repo)] = ask.as_slice() else { panic!("one ask: {ask:?}") };
    assert_eq!(repo, "/w/app");
    assert_eq!(step_says(&view, cx).as_deref(), Some("Reading the run scripts\u{2026}"));

    let (dev, test) = (script("dev", "bun run dev"), script("test", "cargo test --watch"));
    let done = scripts(vec![dev, test.clone()]);
    view.update_in(cx, |v, _w, cx| v.git_done(key, *request, done, cx));
    settle(cx);
    let lines = step_lines(&view, cx);
    let names: Vec<&str> = lines.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["dev", "test"], "the default first");
    assert!(lines[1].1.contains("cargo test --watch"), "its script beside it: {lines:?}");

    cx.simulate_input("test");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let spec = opened(&mut studio);
    let [spec] = spec.as_slice() else { panic!("one terminal: {spec:?}") };
    assert_eq!(
        (&spec.command, spec.cwd.as_deref(), &spec.env, spec.title.as_deref()),
        (&test.command, Some("/w/app"), &test.env, Some("test"))
    );
    assert!(view.read_with(cx, |v, _| v.palette.is_none()), "the step is put away");

    cx.dispatch_action(RunHere);
    settle(cx);
    let names: Vec<String> = step_lines(&view, cx).into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["dev", "test"], "the last read, at once");
    let ask = asked(&mut studio);
    let [(request, _)] = ask.as_slice() else { panic!("asked again: {ask:?}") };
    let done = scripts(vec![script("serve", "bun run serve")]);
    view.update_in(cx, |v, _w, cx| v.git_done(key, *request, done, cx));
    settle(cx);
    let names: Vec<String> = step_lines(&view, cx).into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["serve"], "the fresh list in its place");
    assert_eq!(opened(&mut studio), [], "nothing the person did not pick");
}

/// One script opens as soon as it comes; a repository that keeps none says so in the step, a
/// refusal says why, and a machine that goes before it answers says that.
#[gpui::test]
fn one_script_opens_at_once_and_none_or_a_refusal_is_said(cx: &mut TestAppContext) {
    let (view, cx, mut studio) = in_app(cx);
    let key = studio.key;
    let run = |studio: &mut Fake, cx: &mut VisualTestContext| {
        studio.drain();
        cx.dispatch_action(RunHere);
        settle(cx);
        let ask = asked(studio);
        let [(request, _)] = ask.as_slice() else { panic!("one ask: {ask:?}") };
        *request
    };

    let request = run(&mut studio, cx);
    let none = scripts(Vec::new());
    view.update_in(cx, |v, _w, cx| v.git_done(key, request, none, cx));
    settle(cx);
    assert_eq!(step_says(&view, cx).as_deref(), Some("app keeps no run scripts"));
    cx.simulate_keystrokes("escape");
    settle(cx);

    let request = run(&mut studio, cx);
    let why = "not a git repository".to_owned();
    let refused = GitOutcome::Refused { why: why.clone() };
    view.update_in(cx, |v, _w, cx| v.git_done(key, request, refused, cx));
    settle(cx);
    let said = format!("The run scripts could not be read: {why}");
    assert_eq!(step_says(&view, cx), Some(said));
    cx.simulate_keystrokes("escape");
    settle(cx);

    let request = run(&mut studio, cx);
    let dev = script("dev", "bun run dev");
    let one = scripts(vec![dev.clone()]);
    view.update_in(cx, |v, _w, cx| v.git_done(key, request, one, cx));
    settle(cx);
    let spec = opened(&mut studio);
    let [spec] = spec.as_slice() else { panic!("one terminal: {spec:?}") };
    assert_eq!((&spec.command, spec.title.as_deref()), (&dev.command, Some("dev")));
    assert!(view.read_with(cx, |v, _| v.palette.is_none()), "no step left up");

    cx.dispatch_action(RunHere);
    settle(cx);
    let sent = studio.drain();
    let opens = sent.iter().filter(|m| matches!(m, ClientMsg::OpenSession { .. })).count();
    assert_eq!(opens, 1, "the one read last opens at once");
    let request = sent
        .iter()
        .find_map(|m| match m {
            ClientMsg::Git { request, op: GitOp::Scripts, .. } => Some(*request),
            _ => None,
        })
        .expect("asked again meanwhile");
    let none = scripts(Vec::new());
    view.update_in(cx, |v, _w, cx| v.git_done(key, request, none, cx));
    settle(cx);
    let _gone = run(&mut studio, cx);
    view.update_in(cx, |v, _w, cx| v.disconnect_worker(key, WorkerStatus::Unreachable, cx));
    settle(cx);
    assert_eq!(
        step_says(&view, cx).as_deref(),
        Some("studio went out of reach before its run scripts came")
    );
}

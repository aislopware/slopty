//! Where a shell stands: the directory, repository and branch the worker reports follow a `cd`
//! or a checkout into the session's summary, which the navigator and the status bar read.

use super::*;

/// A cwd event with a branch lands in the summary and on the terminal view; a checkout moves
/// the branch, and a `cd` out of the repository drops both repository and branch.
#[gpui::test]
fn a_cwd_with_a_new_branch_updates_the_summary(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    opens(&view, cx, &fake, session, fake.me, 1);
    let moved = |path: &str, repo: Option<&str>, branch: Option<&str>| TermEvent::Cwd {
        path: path.to_owned(),
        repo: repo.map(str::to_owned),
        branch: branch.map(str::to_owned),
    };
    let place = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| {
            let summary = v.summary(session).expect("the session is known");
            let shown =
                v.terminals.get(&session).and_then(|t| t.read(cx).branch().map(str::to_owned));
            (summary.cwd.clone(), summary.repo.clone(), summary.branch.clone(), shown)
        })
    };
    let owned = |s: &str| Some(s.to_owned());

    view.update_in(cx, |v, _w, cx| {
        v.term_event(session, moved("/w/app", Some("/w"), Some("main")), cx);
    });
    cx.run_until_parked();
    assert_eq!(place(cx), (owned("/w/app"), owned("/w"), owned("main"), owned("main")));

    view.update_in(cx, |v, _w, cx| {
        v.term_event(session, moved("/w/app", Some("/w"), Some("fix")), cx);
    });
    cx.run_until_parked();
    assert_eq!(place(cx), (owned("/w/app"), owned("/w"), owned("fix"), owned("fix")), "a checkout");

    view.update_in(cx, |v, _w, cx| v.term_event(session, moved("/tmp", None, None), cx));
    cx.run_until_parked();
    assert_eq!(place(cx), (owned("/tmp"), None, None, None), "out of the repository");
}

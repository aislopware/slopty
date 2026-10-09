//! A program's status records (`OSC 7501`) as its tile's state.

use slopty_proto::terminal::{ProgramState, ProgramStatus};

use super::*;
use crate::icons::Status;

fn record(id: &str, state: ProgramState, need: Option<&str>) -> ProgramStatus {
    ProgramStatus {
        id: id.to_owned(),
        state,
        need: need.map(str::to_owned),
        progress: None,
        app: String::new(),
        title: String::new(),
        message: String::new(),
    }
}

/// The window shows what a frame drawn from scratch would.
fn fresh(cx: &mut VisualTestContext, step: &str) {
    cx.run_until_parked();
    let stale = cx.update(|window, cx| crate::retained::stale(window, cx, 12));
    if let Some(stale) = stale {
        panic!("{step}: the window shows a stale frame. {stale}");
    }
}

/// A record waiting on the person makes its tile "needs you", said by what it needs; a result
/// is the tile's until the person looks at it, a failed one first; a result that comes while
/// the tile has the focus is looked at already. Each frame is the one drawn from scratch.
#[gpui::test]
fn a_programs_status_is_its_tiles_state(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let (busy, other) = (SessionId::new(), SessionId::new());
    let tile = opens(&view, cx, &fake, busy, fake.me, 1);
    let beside = opens(&view, cx, &fake, other, fake.me, 2);
    let key = fake.key;
    let says = |program: Vec<ProgramStatus>, cx: &mut VisualTestContext| {
        view.update_in(cx, |v, _w, cx| {
            v.session_opened(key, SessionSummary { program, ..summary(busy, None) }, cx);
        });
        fresh(cx, "the records changed");
    };
    let state = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| {
            let item = v.item(tile).expect("the tile").clone();
            let mark = v.tile_status(tile, &item);
            (mark, v.tile_word(&item, mark))
        })
    };
    view.update_in(cx, |v, _w, cx| v.focus_tile(beside, cx));
    fresh(cx, "the other tile focused");
    assert_eq!(state(cx), (None, None));

    let apply = ProgramStatus {
        message: "Apply these changes?".to_owned(),
        ..record("", ProgramState::Blocked, Some(ProgramStatus::PERMISSION))
    };
    says(vec![apply], cx);
    assert_eq!(state(cx), (Some(Status::NeedsYou), Some("Needs approval".to_owned())));
    let words = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.shell_doing(busy).0);
    assert_eq!(words(cx).as_deref(), Some("Apply these changes?"), "its row says what it asks");
    says(vec![record("", ProgramState::Blocked, Some("payment"))], cx);
    assert_eq!(state(cx).1.as_deref(), Some("Needs you"), "a need it does not know");

    let build = record("build", ProgramState::Done, None);
    says(vec![build.clone()], cx);
    assert_eq!(state(cx).0, Some(Status::Done), "a result not looked at");
    let test = record("build/test", ProgramState::Error, None);
    says(vec![build.clone(), test.clone()], cx);
    assert_eq!(state(cx).0, Some(Status::Failed), "a failure first");

    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    fresh(cx, "looked at");
    assert_eq!(state(cx).0, None, "both seen");
    let deploy = record("deploy", ProgramState::Done, None);
    says(vec![build, test, deploy], cx);
    assert_eq!(state(cx).0, None, "a result that came while it was looked at");
    view.update_in(cx, |v, _w, cx| v.focus_tile(beside, cx));
    fresh(cx, "away again");
    assert_eq!(state(cx).0, None, "nothing new since");
}

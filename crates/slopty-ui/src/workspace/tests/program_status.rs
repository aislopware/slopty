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

/// A record waiting on the person climbs the attention ladder as an agent that needs them does:
/// it counts on the bell, sounds once as it comes and not again while it waits, and its note
/// posts while the app is away in the record's words, even with a server leading, which never
/// hears the records. Once it no longer waits, its note is taken back and the bell is empty;
/// waiting again at once, it counts but neither sounds nor notifies again (`PROGRAM_QUIET`).
#[gpui::test]
fn a_program_waiting_on_the_person_notifies_as_an_agent_does(cx: &mut TestAppContext) {
    use std::rc::Rc;

    use slopty_platform::notify::Memory;

    use crate::workspace::attention::Attention;

    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let (busy, other) = (SessionId::new(), SessionId::new());
    let _tile = opens(&view, cx, &fake, busy, fake.me, 1);
    let beside = opens(&view, cx, &fake, other, fake.me, 2);
    view.update_in(cx, |v, _w, cx| v.focus_tile(beside, cx));
    cx.run_until_parked();
    let events = Rc::new(std::cell::RefCell::new(Vec::new()));
    let heard = Rc::clone(&events);
    cx.update(|_w, cx| {
        cx.subscribe(&view, move |_v, e: &WorkspaceEvent, _cx| heard.borrow_mut().push(*e))
            .detach();
    });
    let memory = Rc::new(Memory::default());
    let mut attention = Attention::new(Rc::<Memory>::clone(&memory));
    attention.set_server_led(true);
    attention.set_active(false);
    let key = fake.key;
    let says = |program: Vec<ProgramStatus>, cx: &mut VisualTestContext| {
        view.update_in(cx, |v, _w, cx| {
            v.session_opened(key, SessionSummary { program, ..summary(busy, None) }, cx);
        });
        cx.run_until_parked();
    };
    let look = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.attention_look());
    attention.look(&look(cx));
    assert_eq!(memory.posted(), [], "nothing waits yet");

    let apply = ProgramStatus {
        message: "Apply these changes?".to_owned(),
        ..record("deploy", ProgramState::Blocked, Some(ProgramStatus::PERMISSION))
    };
    says(vec![apply.clone()], cx);
    assert_eq!(view.read_with(cx, |v, _| v.bell_count()), 1, "on the bell");
    let sounds =
        || events.borrow().iter().filter(|e| matches!(e, WorkspaceEvent::Program(_))).count();
    assert_eq!(sounds(), 1, "it sounds");
    assert!(events.borrow().contains(&WorkspaceEvent::NeedsYou(1)), "the badge counts it");
    attention.look(&look(cx));
    let posted = memory.posted();
    let [note] = posted.as_slice() else { panic!("one note: {posted:?}") };
    assert_eq!(note.id, busy.to_string(), "the tile's one note");
    assert_eq!(note.body, "Apply these changes?");
    assert!(note.urgent, "a need breaks through a Focus");

    events.borrow_mut().clear();
    let progress = ProgramStatus { progress: Some(40), ..apply };
    says(vec![progress], cx);
    attention.look(&look(cx));
    assert_eq!(sounds(), 0, "still the same wait: no second sound");
    assert_eq!(memory.posted().len(), 1, "nor a second note");

    says(vec![record("deploy", ProgramState::Working, None)], cx);
    attention.look(&look(cx));
    assert_eq!(view.read_with(cx, |v, _| v.bell_count()), 0, "answered");
    assert_eq!(memory.withdrawn(), [busy.to_string()], "its note taken back");

    events.borrow_mut().clear();
    let again = record("deploy", ProgramState::Blocked, Some(ProgramStatus::QUESTION));
    says(vec![again], cx);
    attention.look(&look(cx));
    assert_eq!(view.read_with(cx, |v, _| v.bell_count()), 1, "waiting again");
    assert_eq!(sounds(), 0, "a record flipping back at once sounds no more");
    assert_eq!(memory.posted().len(), 1, "nor notifies again");
}

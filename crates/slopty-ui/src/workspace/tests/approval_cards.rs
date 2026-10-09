//! The approval cards at the panes' top right: what an agent out of sight waits on.

use gpui::Modifiers;
use slopty_proto::thread::wire::{Intent, RequestCard, TableFrame, ThreadRequest, ThreadRow};
use slopty_proto::thread::{AskId, Choice, Cursor, Effect, Request};

use super::*;
use crate::workspace::approval_cards::{APPROVAL, CARD_W, OPEN_THREAD, QUESTION};

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

/// A thread with no terminal waiting on `ask`, a yes or no it offers plainly when `approval`,
/// else a question.
fn waiting(ask: &str, approval: bool, opened: u64) -> ThreadRow {
    let choice = |id: &str, effect, scope: Option<&str>| Choice {
        id: id.to_owned(),
        label: id.to_owned(),
        effect,
        scope: scope.map(str::to_owned),
        stops: false,
    };
    let mut row = crate::conversation::thread::fixtures::thread("edit").row(WallMs::ZERO);
    row.id = slopty_proto::thread::ThreadId::new();
    row.terminal = None;
    let (kind, options) = if approval {
        let options = vec![
            choice("always", Effect::Allow, Some("this folder")),
            choice("accept", Effect::Allow, None),
            choice("decline", Effect::Deny, None),
        ];
        (Request::APPROVAL, options)
    } else {
        (Request::QUESTION, Vec::new())
    };
    row.requests = vec![RequestCard {
        id: AskId(ask.to_owned()),
        item: None,
        kind: kind.to_owned(),
        title: "Run `cargo test --workspace` in the checkout".to_owned(),
        options,
        opened_ms: WallMs::from_millis(opened),
    }];
    row
}

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
}

fn answers(drained: &[ClientMsg]) -> Vec<Intent> {
    drained
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent {
                intent: i @ Intent::Answer { .. }, ..
            }) => Some(i.clone()),
            _ => None,
        })
        .collect()
}

/// An agent out of sight waiting on a yes or no raises a card at the panes' top right under
/// the title bar, 360 pt wide: "Approval", its request, then Deny and Allow, which answer it
/// with its plain allow, once, and the card goes. A question's card only opens its thread, and
/// the newest card is on top. A card put away does not come back.
#[gpui::test]
fn an_agent_out_of_sight_raises_a_card_that_answers_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let approval = waiting("ask-1", true, 2_000);
    let question = waiting("ask-2", false, 1_000);
    let (yes_no, asks) = (approval.id, question.id);
    table(&view, cx, key, vec![approval, question]);

    let card = cx.debug_bounds(leak(format!("card-{yes_no}"))).expect("the approval's card");
    let other = cx.debug_bounds(leak(format!("card-{asks}"))).expect("the question's card");
    let bar = cx.debug_bounds("titlebar").expect("the title bar");
    let window = cx.update(|window, _| window.viewport_size());
    assert_eq!(card.size.width, px(CARD_W), "360 wide");
    assert!(card.top() > bar.bottom(), "under the title bar");
    let gap = window.width - card.right();
    assert!(gap > px(0.0) && gap <= px(Theme::default().spacing.md), "at the right: {gap:?}");
    assert!(card.bottom() <= other.top(), "the newest on top");
    let said: Vec<String> = tree(cx).into_iter().filter_map(|n| n.label).collect();
    let title = view.read_with(cx, |v, _| v.thread_title(yes_no));
    let words =
        |word: &str| format!("{word}: {title}, Run `cargo test --workspace` in the checkout");
    assert!(said.contains(&words(APPROVAL)), "{said:#?}");
    assert!(said.contains(&words(QUESTION)), "{said:#?}");
    assert!(cx.debug_bounds(leak(format!("card-allow-{asks}"))).is_none(), "a question: none");
    assert!(said.iter().any(|l| l == OPEN_THREAD));

    studio.drain();
    click(cx, leak(format!("card-allow-{yes_no}")));
    assert_eq!(
        answers(&studio.drain()),
        [Intent::Answer {
            ask: AskId("ask-1".to_owned()),
            choice: "accept".to_owned(),
            message: None
        }],
        "the plain allow, once"
    );
    assert!(cx.debug_bounds(leak(format!("card-{yes_no}"))).is_none(), "answered: it goes");

    click(cx, leak(format!("card-open-{asks}")));
    let tile = view.read_with(cx, |v, _| v.tile_of_thread(asks)).expect("its thread opens");
    assert!(view.read_with(cx, |v, _| v.layout.on_show(tile)), "on show");
    assert!(cx.debug_bounds(leak(format!("card-{asks}"))).is_none(), "on show: no card");

    let later = waiting("ask-3", true, 3_000);
    let put = later.id;
    table(&view, cx, key, vec![later]);
    click(cx, leak(format!("card-close-{put}")));
    assert!(cx.debug_bounds(leak(format!("card-{put}"))).is_none(), "put away");
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    assert!(cx.debug_bounds(leak(format!("card-{put}"))).is_none(), "and it does not come back");
}

/// A project muted in the navigator raises no card; a phone has none.
#[gpui::test]
fn a_muted_project_and_a_phone_raise_no_card(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let row = waiting("ask-1", true, 1_000);
    let thread = row.id;
    table(&view, cx, key, vec![row]);
    // Its tile in a tab out of sight, in the shell's project.
    view.update_in(cx, |v, _w, cx| v.open_thread(key, thread, cx));
    cx.run_until_parked();
    let tile = view.read_with(cx, |v, _| v.tile_of_thread(thread)).expect("its tile");
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    let shown = view.read_with(cx, |v, _| v.layout.on_show(tile));
    if shown {
        // Beside the shell: put it in a tab of its own, then show the shell's.
        on_new_tab(&view, cx, tile);
        view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
        cx.run_until_parked();
    }
    assert!(!view.read_with(cx, |v, _| v.layout.on_show(tile)), "out of sight");
    let card = leak(format!("card-{thread}"));
    assert!(cx.debug_bounds(card).is_some(), "a card while its project is not muted");
    let home = view.read_with(cx, |v, _| v.project_groups().group_of(tile).map(|g| g.key.clone()));
    let home = home.expect("its project");
    view.update_in(cx, |v, _w, cx| v.toggle_muted(&home, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(card).is_none(), "muted: no card");
    view.update_in(cx, |v, _w, cx| v.toggle_muted(&home, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(card).is_some());

    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds(card).is_none(), "a phone has none");
}

//! Asking aside: a fork of the thread in a sheet over it, the draft its question, ended for
//! good when the sheet closes, or kept as a thread of its own.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Modifiers, TestAppContext};
use slopty_proto::ClientMsg;
use slopty_proto::thread::wire::{Intent, IntentDone, Outcome, ThreadRequest};
use slopty_proto::thread::{Cap, Delivery, IntentId, ThreadId};

use super::{Sent, hub, snapshot, view};
use crate::conversation::thread::fixtures;
use crate::conversation::thread::hub::{HubEvent, ThreadHub};

/// Every intent sent, with its thread and id.
fn sent_intents(sent: &Sent) -> Vec<(IntentId, ThreadId, Intent)> {
    sent.borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent { id, thread, intent }) => {
                Some((*id, *thread, intent.clone()))
            }
            _ => None,
        })
        .collect()
}

fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} drawn")).center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

/// A thread whose agent forks, with one message of the person's, `u`.
fn forking() -> slopty_proto::thread::ThreadState {
    use slopty_core::WallMs;
    use slopty_proto::thread::{Clipped, Item, ItemBody, ItemId, TurnId, UserMessage};

    let mut state = fixtures::empty();
    state.meta.caps = vec![Cap::named(Cap::FORK), Cap::named(Cap::STEER)];
    state.items = vec![Item {
        id: ItemId("u".to_owned()),
        turn: TurnId(1),
        at_ms: WallMs::ZERO,
        body: ItemBody::User(UserMessage {
            text: Clipped::whole("Speed up the parser"),
            images: Vec::new(),
            command: None,
            intent: None,
        }),
    }];
    state
}

/// "Ask aside" from the message's own menu.
fn ask_aside_from_menu(cx: &mut gpui::VisualTestContext) {
    let message = cx.debug_bounds("item-u").expect("the message").center();
    cx.simulate_mouse_down(message, gpui::MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    click(cx, "message-menu-aside");
}

/// The draft is asked aside from a message's menu: the thread forks, the sheet opens over
/// it, and the question goes to the fork once it is there. Closing the sheet ends the fork for
/// good.
#[gpui::test]
fn an_aside_asks_beside_the_thread_and_closes_for_good(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let state = forking();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();

    cx.simulate_input("Why is the parser slow?");
    ask_aside_from_menu(cx);
    let asked = sent_intents(&sent);
    let [(id, on, Intent::Aside)] = asked.as_slice() else { panic!("{asked:?}") };
    assert_eq!(*on, thread);
    assert!(cx.debug_bounds("thread-aside").is_some(), "the sheet, while it forks");

    let fork = ThreadId::new();
    let done = IntentDone { id: *id, outcome: Outcome::Started { thread: fork } };
    hub.update(cx, |hub, cx| hub.done(&done, cx));
    cx.run_until_parked();
    let question = Intent::Send {
        text: "Why is the parser slow?".to_owned(),
        delivery: Delivery::Steer,
        attachments: vec![],
    };
    assert_eq!(sent_intents(&sent).last().map(|(_, t, i)| (*t, i.clone())), Some((fork, question)));
    assert!(cx.debug_bounds("aside-keep").is_some(), "the fork can be kept");

    click(cx, "aside-close");
    assert_eq!(
        sent_intents(&sent).last().map(|(_, t, i)| (*t, i.clone())),
        Some((fork, Intent::Discard))
    );
    assert!(cx.debug_bounds("thread-aside").is_none(), "the sheet goes");
    ask_aside_from_menu(cx);
    assert_eq!(sent_intents(&sent).last().map(|(_, _, i)| i.clone()), Some(Intent::Aside));
}

/// An aside asked from the palette, kept, loses its mark, and once the worker says so the
/// workspace is asked to open it as a thread of its own.
#[gpui::test]
fn a_kept_aside_becomes_a_thread(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.caps = vec![Cap::named(Cap::FORK)];
    let opened: Rc<RefCell<Vec<(ThreadId, bool)>>> = Rc::default();
    let into = Rc::clone(&opened);
    cx.update(|cx| {
        cx.subscribe(&hub, move |_hub, event: &HubEvent, _cx| {
            if let HubEvent::Started { thread, aside, .. } = event {
                into.borrow_mut().push((*thread, *aside));
            }
        })
        .detach();
    });
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();

    cx.dispatch_action(crate::conversation::AskAside);
    cx.run_until_parked();
    let (id, ..) = sent_intents(&sent).pop().expect("asked from the palette");
    let fork = ThreadId::new();
    let done = IntentDone { id, outcome: Outcome::Started { thread: fork } };
    hub.update(cx, |hub, cx| hub.done(&done, cx));
    cx.run_until_parked();
    assert_eq!(*opened.borrow(), [(fork, true)], "an aside is the view's to show");

    let mut forked = fixtures::empty();
    forked.meta.id = fork;
    forked.meta.caps = vec![Cap::named(Cap::FORK)];
    forked
        .meta
        .facts
        .insert(slopty_proto::thread::ThreadMeta::ASIDE_FACT.to_owned(), thread.to_string());
    hub.update(cx, |hub, cx| hub.frame(fork, snapshot(forked, 1), cx));
    cx.run_until_parked();

    click(cx, "aside-keep");
    let (keep, on, intent) = sent_intents(&sent).pop().expect("kept");
    assert_eq!((on, intent), (fork, Intent::KeepAside));
    assert!(cx.debug_bounds("thread-aside").is_none(), "the sheet goes");
    let done = IntentDone { id: keep, outcome: Outcome::Done };
    hub.update(cx, |hub, cx| hub.done(&done, cx));
    cx.run_until_parked();
    assert_eq!(opened.borrow().last(), Some(&(fork, false)), "the workspace opens it");
}

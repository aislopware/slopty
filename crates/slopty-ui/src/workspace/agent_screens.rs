//! The screens agents drive, as tiles beside their threads.
//!
//! A thread view asks to watch the screen its agent drives ([`ThreadViewEvent::Watch`]): the
//! window or display opens as a stream tile on the agent's worker, or the tile already showing
//! it is gone to, and the item carries the thread in [`AGENT_FACT`], so every client that shows
//! it knows whose screen it is. Each frame, the stream of such a tile is told its driver
//! ([`ScreenView::set_driver`]) from the thread's row: the agent, and whether its turn is under
//! way. The view watches until the person takes control, which stops that turn through the
//! thread's own door ([`Intent::Interrupt`]).
//!
//! [`ThreadViewEvent::Watch`]: crate::conversation::thread::view::ThreadViewEvent::Watch

use std::rc::Rc;

use gpui::{App, Context, Entity};
use slopty_core::ItemId;
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::screen::CaptureTarget;
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{AgentScreen, Phase, ThreadId};

use super::{WorkerKey, WorkspaceView};
use crate::conversation::thread::hub::ThreadHub;
use crate::screen::{Driver, ScreenView};

/// The fact of a window or display item that an agent drives: its thread, by id.
pub const AGENT_FACT: &str = "slopty.agent-thread";

/// Whether item `kind` shows `target`.
fn shows(kind: &ItemKind, target: CaptureTarget) -> bool {
    match (kind, target) {
        (ItemKind::Window { window }, CaptureTarget::Window(want)) => *window == want,
        (ItemKind::Display { display }, CaptureTarget::Display(want)) => *display == want,
        _ => false,
    }
}

impl WorkspaceView {
    /// Show `screen`, which `thread`'s agent on `key` drives: go to the tile showing it, marked
    /// as the agent's, else open one beside the focus.
    pub(super) fn watch_agent_screen(
        &mut self,
        key: WorkerKey,
        thread: ThreadId,
        screen: &AgentScreen,
        cx: &mut Context<Self>,
    ) {
        let fact = thread.to_string();
        let open = self.workers.get(&key).and_then(|w| {
            w.doc
                .items()
                .find(|i| shows(&i.kind, screen.target))
                .map(|i| (i.id, i.facts.get(AGENT_FACT) == Some(&fact)))
        });
        if let Some((id, marked)) = open {
            if !marked {
                let set = ItemOp::SetFact { id, key: AGENT_FACT.to_owned(), value: Some(fact) };
                self.propose(key, set, cx);
            }
            self.go_to(id, cx);
            return;
        }
        let kind = match screen.target {
            CaptureTarget::Window(window) => ItemKind::Window { window },
            CaptureTarget::Display(display) => ItemKind::Display { display },
        };
        let item = Item {
            id: ItemId::new(),
            kind,
            name: None,
            facts: std::iter::once((AGENT_FACT.to_owned(), fact)).collect(),
        };
        tracing::info!(id = %item.id, %thread, target = ?screen.target, "watch an agent's screen");
        self.titles.insert(item.id, screen.label.clone());
        self.propose(key, ItemOp::Add(item), cx);
    }

    /// Tell each stream of a screen an agent drives who drives it now; a stream no thread
    /// drives has no driver.
    pub(super) fn settle_agent_screens(&self, cx: &mut Context<Self>) {
        let driven: Vec<(ItemId, Entity<ScreenView>, Option<Driver>)> = self
            .items()
            .filter_map(|(key, item)| {
                let view = self.screens.get(&item.id)?.clone();
                let thread = item.facts.get(AGENT_FACT).and_then(|t| t.parse::<ThreadId>().ok());
                let hub = self.held_hub(key);
                let driver = thread.zip(hub).and_then(|(thread, hub)| driver(hub, thread, cx));
                Some((item.id, view, driver))
            })
            .collect();
        for (_id, view, driver) in driven {
            let same = match (view.read(cx).driver(), &driver) {
                (Some(was), Some(now)) => was.same(now),
                (None, None) => true,
                _ => false,
            };
            if !same {
                view.update(cx, |view, cx| view.set_driver(driver, cx));
            }
        }
    }
}

/// The driver `thread` on `hub` is, as its row says: its agent, whether it works, and the stop
/// of its turn for when the person takes control.
fn driver(hub: &Entity<ThreadHub>, thread: ThreadId, cx: &App) -> Option<Driver> {
    let row = hub.read(cx).threads().rows().rows.get(&thread)?;
    let agent = row.agent.clone();
    let name = crate::conversation::thread::view::agent_label(&agent);
    let working = row.status.phase == Phase::Working;
    let hub = hub.downgrade();
    let take = Rc::new(move |cx: &mut App| {
        let _gone = hub.update(cx, |hub, cx| {
            let state = hub.threads().rows().rows.get(&thread).map(|r| r.status.phase);
            if state == Some(Phase::Working) && !hub.threads().stopping(thread) {
                let _id = hub.intent(thread, Intent::Interrupt, cx);
            }
        });
    });
    Some(Driver { agent, name, working, take })
}

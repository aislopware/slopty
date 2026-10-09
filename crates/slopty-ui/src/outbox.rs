//! A connection's outbound queue with a line in front of it.
//!
//! Everything on a connection to a worker shares one bounded queue, and the terminal and
//! screen views fill it on purpose while they stream. A message that found it full used to be
//! dropped and reported sent: a thread's intent, a terminal's attach, a file's save. Through an
//! [`Outbox`] a message that finds the queue full waits, in order, for room instead, and a task
//! moves it in as room frees. What happens to a message that has to wait is the outbox's
//! policy ([`Hold`]): one that only the latest of its kind matters for takes the place of the
//! one waiting, and nothing else is dropped.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use gpui::App;
use slopty_proto::ClientMsg;
use slopty_proto::screen::ScreenRequest;
use slopty_proto::terminal::TermRequest;
use tokio::sync::{Notify, mpsc};

/// What becomes of `msg` that has to wait behind `waiting`: queued at the back, in place of
/// one it outdates, or dropped.
pub(crate) type Hold = fn(&mut VecDeque<ClientMsg>, ClientMsg);

/// A connection's outbound queue `out`, and what waits in order for room in it.
pub(crate) struct Outbox {
    out: mpsc::Sender<ClientMsg>,
    waiting: Rc<RefCell<VecDeque<ClientMsg>>>,
    wake: Rc<Notify>,
    hold: Hold,
}

impl Outbox {
    /// An outbox into `out` that holds what waits by `hold`, with the task that moves it into
    /// the queue as room frees. The task outlives the outbox until what it left waiting has
    /// gone, or the connection has.
    pub(crate) fn new(out: mpsc::Sender<ClientMsg>, hold: Hold, cx: &App) -> Self {
        let waiting = Rc::default();
        let wake = Rc::new(Notify::new());
        cx.foreground_executor()
            .spawn(flush(out.clone(), Rc::clone(&waiting), Rc::clone(&wake)))
            .detach();
        Self { out, waiting, wake, hold }
    }

    /// Send `msg`: into the queue now when nothing waits and it has room, else behind what
    /// waits. False once the connection is gone.
    pub(crate) fn send(&self, msg: ClientMsg) -> bool {
        let mut waiting = self.waiting.borrow_mut();
        if waiting.is_empty() {
            match self.out.try_send(msg) {
                Ok(()) => return true,
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!("outbound queue closed");
                    return false;
                }
                Err(mpsc::error::TrySendError::Full(msg)) => waiting.push_back(msg),
            }
        } else {
            (self.hold)(&mut waiting, msg);
        }
        self.wake.notify_one();
        true
    }

    /// Whether nothing waits for room.
    pub(crate) fn is_clear(&self) -> bool {
        self.waiting.borrow().is_empty()
    }
}

impl Drop for Outbox {
    fn drop(&mut self) {
        // The flush task ends once it holds the last reference and nothing waits.
        self.wake.notify_one();
    }
}

/// Move what waits into the queue in order as room frees, until the outbox is gone and nothing
/// waits, or the connection is.
async fn flush(
    out: mpsc::Sender<ClientMsg>,
    waiting: Rc<RefCell<VecDeque<ClientMsg>>>,
    wake: Rc<Notify>,
) {
    loop {
        while !waiting.borrow().is_empty() {
            let Ok(permit) = out.reserve().await else { return };
            let Some(msg) = waiting.borrow_mut().pop_front() else { break };
            permit.send(msg);
        }
        if Rc::strong_count(&waiting) == 1 {
            return;
        }
        wake.notified().await;
    }
}

/// The workspace's policy for what it sends a worker: everything arrives, in order, except
/// that a message only the latest of which matters (a whole watch set, the handoffs taken, a
/// size, a probe) takes the place of the one of its kind waiting, at the back.
pub(crate) fn hold_control(waiting: &mut VecDeque<ClientMsg>, msg: ClientMsg) {
    if let Some(kind) = Latest::of(&msg) {
        waiting.retain(|w| Latest::of(w) != Some(kind));
    }
    waiting.push_back(msg);
}

/// A message only the latest of which matters, by what it sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Latest {
    /// The whole set of files watched.
    WatchFiles,
    /// The whole set of folders watched.
    WatchFolders,
    /// The handoffs this client takes.
    HandoffCaps,
    /// A liveness probe: one waiting says as much as several.
    Ping,
    /// A terminal's size.
    TermSize(slopty_core::SessionId),
    /// A remote display's size.
    ScreenSize(slopty_core::StreamId),
}

impl Latest {
    const fn of(msg: &ClientMsg) -> Option<Self> {
        Some(match msg {
            ClientMsg::WatchFiles { .. } => Self::WatchFiles,
            ClientMsg::WatchFolders { .. } => Self::WatchFolders,
            ClientMsg::HandoffCaps(_) => Self::HandoffCaps,
            ClientMsg::Ping { .. } => Self::Ping,
            ClientMsg::Term { session, req: TermRequest::Resize(_) } => Self::TermSize(*session),
            ClientMsg::Screen(ScreenRequest::Resize { stream, .. }) => Self::ScreenSize(*stream),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::SessionId;
    use slopty_proto::terminal::TermSize;

    use super::*;

    fn watch(path: &str) -> ClientMsg {
        ClientMsg::WatchFiles { paths: vec![path.to_owned()] }
    }

    fn read(path: &str) -> ClientMsg {
        ClientMsg::ReadFile { path: path.to_owned() }
    }

    fn resize(session: SessionId, cols: u16) -> ClientMsg {
        ClientMsg::Term {
            session,
            req: TermRequest::Resize(TermSize { cols, rows: 24, ..TermSize::default() }),
        }
    }

    /// Behind a full queue, a watch set or a size takes the place of the one of its kind
    /// waiting, at the back; anything else queues in order, however much waits.
    #[test]
    fn what_waits_keeps_everything_but_an_outdated_latest() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let mut waiting = VecDeque::new();
        for msg in [watch("/a"), read("/x"), resize(a, 80), resize(b, 90), read("/y")] {
            hold_control(&mut waiting, msg);
        }
        for msg in [watch("/b"), resize(a, 100)] {
            hold_control(&mut waiting, msg);
        }
        assert_eq!(
            Vec::from(waiting),
            [read("/x"), resize(b, 90), read("/y"), watch("/b"), resize(a, 100)]
        );
        let mut many = VecDeque::new();
        for n in 0..10_000 {
            hold_control(&mut many, read(&format!("/{n}")));
        }
        assert_eq!(many.len(), 10_000, "nothing that must arrive is dropped");
    }
}

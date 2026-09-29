//! A shell's web pages and edits, handed to the client in front of it
//! (`slopty_worker::handoff` keeps who that is and whether a page opens or is offered).
//!
//! Only clients that said they take a kind of handoff are asked. With none connected, or none
//! that takes it, the program is answered at once ([`Handed::Nobody`]) and falls back to this
//! machine: the system's opener, or `vi`.
//!
//! **A page** ([`open`]) is asked of each candidate in turn, each given [`TAKE_WAIT`] to say it
//! opened or offered it; one that does not answer in time is withdrawn and the next is asked.
//!
//! **An edit** ([`edit`]) is asked the same way. A client that takes it shows the file in a
//! tile; the program that asked (`EDITOR`, `slopty edit --wait`) is answered only when the
//! person is done, or when the client has been gone for [`LOST_AFTER`] (a relaunched app is
//! back well within it and asked again under the same number). The program giving up (its
//! end of the control socket closing) withdraws the edit from the tile.
//!
//! **Presence** ([`presence`]): each session's presence file follows whether any client is
//! focused on it, written in order on a task of its own, off the connections' loops.

use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use slopty_core::{ClientId, SessionId, WallMs};
use slopty_proto::ctl::{EditAsk, Handed, NoClient};
use slopty_proto::handoff::{EditFile, HandoffEvent, HandoffId, HandoffReply, OpenUrl, Page};
use slopty_worker::handoff::{Heard, Need, Nobody};
use tokio::sync::mpsc;

use crate::Daemon;

/// How long one client has to say it took a handoff before it is withdrawn and the next is
/// asked. An app opens a page or a tile in well under this; a wedged one is passed over.
pub const TAKE_WAIT: Duration = Duration::from_secs(3);

/// How long a waiting edit outlives its client's connection: a relaunched app, or a phone back
/// from the background, reconnects well within it.
pub const LOST_AFTER: Duration = Duration::from_mins(5);

/// Open `page` for a program in `session` in the browser of the client in front of it, or
/// offer it there ([`slopty_worker::handoff::Handoffs::open_as`]).
pub async fn open(daemon: &Daemon, session: Option<SessionId>, page: Page) -> Handed {
    let (id, candidates) = {
        let mut handoffs = daemon.handoffs.lock();
        (handoffs.next_id(), handoffs.candidates(session, Need::Open))
    };
    let candidates = match candidates {
        Ok(candidates) => candidates,
        Err(nobody) => return nobody_for(nobody),
    };
    let (waiter, mut heard) = mpsc::unbounded_channel();
    let mut missed = Missed::default();
    for client in candidates {
        {
            let mut handoffs = daemon.handoffs.lock();
            let offer = handoffs.open_as(&page, session, client, Instant::now());
            let open =
                OpenUrl { id, session, url: page.url.clone(), asked_ms: WallMs::now(), offer };
            if !handoffs.ask(client, id, HandoffEvent::Open(open), &waiter) {
                continue;
            }
        }
        match answer(&mut heard, client).await {
            Answer::Took(by, reply) => {
                let client = finish(daemon, id, by);
                return match reply {
                    HandoffReply::Offered { why, .. } => Handed::Offered { client, why },
                    _ => Handed::Taken { client },
                };
            }
            Answer::No(why) => missed.note(daemon, id, client, why),
        }
    }
    daemon.handoffs.lock().finish(id, None);
    Handed::Nobody { why: missed.why() }
}

/// Show `ask`'s file in a tile of the client in front of its session, and when it waits, answer
/// once the person is done. `gave_up` finishes when the program that asked goes away.
pub async fn edit(daemon: &Daemon, ask: EditAsk, gave_up: impl Future<Output = ()>) -> Handed {
    let (id, candidates) = {
        let mut handoffs = daemon.handoffs.lock();
        (handoffs.next_id(), handoffs.candidates(ask.session, Need::Edit))
    };
    let candidates = match candidates {
        Ok(candidates) => candidates,
        Err(nobody) => return nobody_for(nobody),
    };
    let EditAsk { session, path, line, wait } = ask;
    let edit = EditFile { id, session, path, line, wait };
    let (waiter, mut heard) = mpsc::unbounded_channel();
    let mut missed = Missed::default();
    let mut holder = None;
    for client in candidates {
        if !daemon.handoffs.lock().ask(client, id, HandoffEvent::Edit(edit.clone()), &waiter) {
            continue;
        }
        match answer(&mut heard, client).await {
            Answer::Took(by, _reply) => {
                holder = Some(by);
                break;
            }
            Answer::No(why) => missed.note(daemon, id, client, why),
        }
    }
    let Some(holder) = holder else {
        daemon.handoffs.lock().finish(id, None);
        return Handed::Nobody { why: missed.why() };
    };
    if !wait {
        return Handed::Taken { client: finish(daemon, id, holder) };
    }
    daemon.handoffs.lock().hold(holder, edit);
    let handed = until_edited(&mut heard, holder, gave_up).await;
    let taker = matches!(handed, Some(Handed::Edited { .. })).then_some(holder);
    daemon.handoffs.lock().finish(id, taker);
    handed.unwrap_or(Handed::Lost)
}

/// End handoff `id`, taken by `taker`: the others asked are withdrawn. The taker's name.
fn finish(daemon: &Daemon, id: HandoffId, taker: ClientId) -> String {
    let name = {
        let mut handoffs = daemon.handoffs.lock();
        let name = handoffs.name(taker).unwrap_or("a client").to_owned();
        handoffs.finish(id, Some(taker));
        name
    };
    tracing::info!(%id, client = %name, "handed over");
    name
}

const fn nobody_for(nobody: Nobody) -> Handed {
    Handed::Nobody {
        why: match nobody {
            Nobody::Connected => NoClient::NoneConnected,
            Nobody::Capable => NoClient::NoneCapable,
        },
    }
}

/// How the asked clients that did not take a handoff failed, for the reason given.
#[derive(Default)]
struct Missed {
    refused: usize,
    timed_out: usize,
}

impl Missed {
    fn note(&mut self, daemon: &Daemon, id: HandoffId, client: ClientId, why: No) {
        tracing::debug!(%id, %client, ?why, "not taken; asking the next client");
        match why {
            No::Refused => self.refused = self.refused.saturating_add(1),
            No::TimedOut => {
                self.timed_out = self.timed_out.saturating_add(1);
                daemon.handoffs.lock().withdraw(id, client);
            }
            No::Gone => {}
        }
    }

    const fn why(&self) -> NoClient {
        if self.timed_out > 0 {
            NoClient::TimedOut
        } else if self.refused > 0 {
            NoClient::Refused
        } else {
            NoClient::NoneConnected
        }
    }
}

/// How an asked client answered.
enum Answer {
    /// This client, or one asked before it answering late, took it (opened, offered, shown).
    Took(ClientId, HandoffReply),
    /// It did not.
    No(No),
}

#[derive(Clone, Copy, Debug)]
enum No {
    Refused,
    TimedOut,
    Gone,
}

/// Wait up to [`TAKE_WAIT`] for `asked` to answer. A take from any client asked for this
/// handoff counts, since its page is open or its tile shows; a refusal or a disconnect counts
/// only from `asked`.
async fn answer(heard: &mut mpsc::UnboundedReceiver<Heard>, asked: ClientId) -> Answer {
    let waited = tokio::time::timeout(TAKE_WAIT, async {
        loop {
            match heard.recv().await {
                Some(Heard::Reply(
                    by,
                    reply @ (HandoffReply::Taken { .. } | HandoffReply::Offered { .. }),
                )) => return Answer::Took(by, reply),
                Some(Heard::Reply(by, HandoffReply::Refused { .. })) if by == asked => {
                    return Answer::No(No::Refused);
                }
                Some(Heard::Gone(by)) if by == asked => return Answer::No(No::Gone),
                None => return Answer::No(No::Gone),
                Some(_) => {}
            }
        }
    })
    .await;
    waited.unwrap_or(Answer::No(No::TimedOut))
}

/// Wait on the edit `holder` took: `Some` with how it ended, `None` when the program gave up.
async fn until_edited(
    heard: &mut mpsc::UnboundedReceiver<Heard>,
    holder: ClientId,
    gave_up: impl Future<Output = ()>,
) -> Option<Handed> {
    let mut gave_up = std::pin::pin!(gave_up);
    let mut lost_at: Option<tokio::time::Instant> = None;
    loop {
        let lost = async {
            match lost_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = &mut gave_up => return None,
            () = lost => return Some(Handed::Lost),
            heard = heard.recv() => match heard {
                Some(Heard::Reply(by, HandoffReply::Edited { outcome, .. })) if by == holder => {
                    return Some(Handed::Edited { outcome });
                }
                Some(Heard::Gone(by)) if by == holder => {
                    lost_at = tokio::time::Instant::now().checked_add(LOST_AFTER);
                }
                Some(Heard::Back(by)) if by == holder => lost_at = None,
                Some(_) => {}
                None => return Some(Handed::Lost),
            },
        }
    }
}

/// Keep each session's presence file as focus says, in the order focus changed, until the
/// daemon stops. Starts from an empty directory: nobody is focused on anything yet.
pub async fn presence(dir: PathBuf, mut changes: mpsc::UnboundedReceiver<(SessionId, bool)>) {
    let fresh = dir.clone();
    let cleared = tokio::task::spawn_blocking(move || {
        match std::fs::remove_dir_all(&fresh) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::fs::create_dir_all(&fresh)
    })
    .await;
    if !matches!(cleared, Ok(Ok(()))) {
        tracing::warn!(dir = %dir.display(), "presence files could not be reset");
    }
    let mut batch = Vec::new();
    while changes.recv_many(&mut batch, 64).await > 0 {
        let dir = dir.clone();
        let marks = std::mem::take(&mut batch);
        let done = tokio::task::spawn_blocking(move || {
            for (session, present) in marks {
                if let Err(e) = slopty_worker::handoff::mark_presence(&dir, session, present) {
                    tracing::warn!(%session, present, error = %e, "presence file");
                }
            }
        })
        .await;
        if done.is_err() {
            tracing::warn!("presence files: the blocking pool is gone");
        }
    }
}

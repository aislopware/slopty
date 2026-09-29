//! What a client does when a program in a worker's shell hands it a web page or a file
//! (`slopty_proto::handoff`), and what it answers.
//!
//! A client that takes handoffs says so once its link is up, and again when that changes, with
//! [`declare`]; the worker asks nobody else. The UI then feeds each [`HandoffEvent`] it receives
//! (`LinkEvent::Control(WorkerMsg::Handoff)`) to [`Handoffs::heard`] and does what the [`Todo`]
//! says, sending the answer it carries.
//!
//! This is the machine a page would open on, so it checks the page again whatever the worker
//! did: an address that is not a plain web page is refused, and one to be wary of, one the
//! worker said to offer, or one that arrived late is offered in a notice showing its host, never
//! opened unasked. A waiting edit is remembered until the person is done with it
//! ([`Handoffs::edited`]) or the worker withdraws it; an edit asked again after a reconnect is
//! recognised by its number and only answered.

use std::collections::HashMap;
use std::time::Duration;

use slopty_core::WallMs;
use slopty_proto::ClientMsg;
use slopty_proto::handoff::{
    EditFile, EditOutcome, HandoffCaps, HandoffEvent, HandoffId, HandoffReply, OfferReason,
    OpenUrl, page,
};

/// A page asked for longer ago than this, by the worker's clock, is offered rather than opened.
///
/// The worker gives each client three seconds to answer before it withdraws the page and asks
/// another. A page that arrives later than this might be answered too late, and open on two
/// machines; the second left over is for the answer's way back.
pub const LATE_AFTER: Duration = Duration::from_secs(2);

/// The message that tells a worker which handoffs this client takes: send it right after the
/// link is up, and again when it changes.
#[must_use]
pub const fn declare(open: bool, edit: bool) -> ClientMsg {
    ClientMsg::HandoffCaps(HandoffCaps { open, edit })
}

/// What to do about one handoff.
#[derive(Clone, PartialEq, Debug)]
pub enum Todo {
    /// Open `url` in the person's browser now (the platform's opener), then send `reply`.
    Open {
        /// The page, as parsed.
        url: String,
        /// "Taken", once opened.
        reply: ClientMsg,
    },
    /// Offer the page: a notice naming `host` (and `open.session`, and when it was asked) with
    /// an "Open" action that opens `url`, and why it was not opened (`why`). Send `reply` as
    /// soon as the notice shows; opening it later sends nothing more.
    Offer {
        /// The page, as parsed: what "Open" opens.
        url: String,
        /// Its host as a browser reads it, ASCII (`xn--` for an internationalised name).
        host: String,
        /// The ask.
        open: OpenUrl,
        /// Why it is offered.
        why: OfferReason,
        /// "Offered".
        reply: ClientMsg,
    },
    /// Show `edit.path` in a file tile beside `edit.session`'s tile (at `edit.line`), focused,
    /// then send `reply`. `again` when the tile already shows it (the worker asked again after
    /// a reconnect): only bring it forward.
    Edit {
        /// The file and whether a program waits on it.
        edit: EditFile,
        /// "Taken", once the tile shows it.
        reply: ClientMsg,
        /// The tile is already there.
        again: bool,
    },
    /// Send `reply` and do nothing else: this client cannot take it.
    Refuse {
        /// "Refused".
        reply: ClientMsg,
    },
    /// Forget handoff `id`: do not open it if it has not opened yet, dismiss its notice, or
    /// stop the tile waiting (its program went away; the tile stays, as an ordinary file tile).
    Withdraw {
        /// The handoff.
        id: HandoffId,
    },
}

/// This client's handoffs from one worker: the edits a program waits on.
#[derive(Debug, Default)]
pub struct Handoffs {
    waiting: HashMap<HandoffId, EditFile>,
}

impl Handoffs {
    /// What to do about `event`, heard at `now`; `can_edit` when this client shows files now.
    pub fn heard(&mut self, event: HandoffEvent, can_edit: bool, now: WallMs) -> Todo {
        match event {
            HandoffEvent::Open(open) => {
                let Some(page) = page(&open.url) else {
                    tracing::warn!(url = %open.url, "a worker asked to open something that is not a web page");
                    return Todo::Refuse { reply: reply(HandoffReply::Refused { id: open.id }) };
                };
                let late = now.since(open.asked_ms) > LATE_AFTER;
                let why = page
                    .wary
                    .map(OfferReason::Wary)
                    .or(open.offer)
                    .or_else(|| late.then_some(OfferReason::Late));
                match why {
                    Some(why) => Todo::Offer {
                        url: page.url,
                        host: page.host,
                        reply: reply(HandoffReply::Offered { id: open.id, why }),
                        open,
                        why,
                    },
                    None => Todo::Open {
                        url: page.url,
                        reply: reply(HandoffReply::Taken { id: open.id }),
                    },
                }
            }
            HandoffEvent::Edit(edit) if !can_edit => {
                Todo::Refuse { reply: reply(HandoffReply::Refused { id: edit.id }) }
            }
            HandoffEvent::Edit(edit) => {
                let id = edit.id;
                let again = self.waiting.contains_key(&id);
                if edit.wait {
                    self.waiting.insert(id, edit.clone());
                }
                Todo::Edit { edit, reply: reply(HandoffReply::Taken { id }), again }
            }
            HandoffEvent::Withdrawn { id } => {
                self.waiting.remove(&id);
                Todo::Withdraw { id }
            }
        }
    }

    /// The person is done with waiting edit `id` (closed its tile, or marked it done, or gave
    /// it up): the answer to send once the tile's last save was answered (`WorkerMsg::Written`),
    /// or `None` for an edit nothing waits on.
    pub fn edited(&mut self, id: HandoffId, outcome: EditOutcome) -> Option<ClientMsg> {
        self.waiting.remove(&id)?;
        Some(reply(HandoffReply::Edited { id, outcome }))
    }

    /// The waiting edit showing `path`, if any: what a file tile checks when it closes.
    #[must_use]
    pub fn waiting_on(&self, path: &str) -> Option<&EditFile> {
        self.waiting.values().find(|edit| edit.path == path)
    }
}

const fn reply(reply: HandoffReply) -> ClientMsg {
    ClientMsg::Handoff(reply)
}

#[cfg(test)]
mod tests {
    use slopty_proto::handoff::Wary;

    use super::*;

    const NOW: WallMs = WallMs::from_millis(1_790_000_000_000);

    fn open(id: HandoffId, url: &str, offer: Option<OfferReason>) -> HandoffEvent {
        HandoffEvent::Open(OpenUrl { id, session: None, url: url.to_owned(), asked_ms: NOW, offer })
    }

    fn edit(id: HandoffId, wait: bool) -> EditFile {
        EditFile { id, session: None, path: "/r/.git/COMMIT_EDITMSG".to_owned(), line: None, wait }
    }

    fn offered(todo: &Todo) -> Option<(&str, &str, OfferReason)> {
        match todo {
            Todo::Offer { url, host, why, .. } => Some((url, host, *why)),
            _ => None,
        }
    }

    /// A web page the worker says to open opens; one it says to offer, one this client finds
    /// wary of, and one that arrived late are offered naming the host a browser would visit;
    /// anything but a web page is refused whatever the worker let through.
    #[test]
    fn a_page_opens_is_offered_or_is_refused() {
        let mut h = Handoffs::default();
        assert_eq!(
            h.heard(open(1, "https://github.com/login/device", None), true, NOW),
            Todo::Open {
                url: "https://github.com/login/device".to_owned(),
                reply: ClientMsg::Handoff(HandoffReply::Taken { id: 1 }),
            }
        );
        let asked = h.heard(open(2, "https://x.test/", Some(OfferReason::NotTyped)), true, NOW);
        assert_eq!(offered(&asked), Some(("https://x.test/", "x.test", OfferReason::NotTyped)));
        assert!(
            matches!(&asked, Todo::Offer { reply, .. } if *reply == ClientMsg::Handoff(HandoffReply::Offered { id: 2, why: OfferReason::NotTyped }))
        );
        let tricky = h.heard(open(3, "https://github.com@evil.test/", None), true, NOW);
        assert_eq!(
            offered(&tricky),
            Some(("https://github.com@evil.test/", "evil.test", OfferReason::Wary(Wary::UserInfo)))
        );
        let local = h.heard(open(4, "http://localhost:5173/", None), true, NOW);
        assert_eq!(offered(&local).map(|o| o.2), Some(OfferReason::Wary(Wary::Loopback)));
        let later = NOW.saturating_add(LATE_AFTER + Duration::from_millis(1));
        let late = h.heard(open(5, "https://github.com/", None), true, later);
        assert_eq!(offered(&late).map(|o| o.2), Some(OfferReason::Late));
        assert!(
            matches!(
                h.heard(open(6, "https://github.com/", None), true, NOW.saturating_add(LATE_AFTER)),
                Todo::Open { .. }
            ),
            "on time"
        );
        assert_eq!(
            h.heard(
                open(7, "x-apple.systempreferences:com.apple.preference.security", None),
                true,
                NOW
            ),
            Todo::Refuse { reply: ClientMsg::Handoff(HandoffReply::Refused { id: 7 }) }
        );
        assert_eq!(
            declare(true, false),
            ClientMsg::HandoffCaps(HandoffCaps { open: true, edit: false })
        );
    }

    /// A waiting edit is kept until the person is done, recognised when asked again, and
    /// forgotten when withdrawn; a client without file tiles refuses it; an edit nothing waits
    /// on needs no ending.
    #[test]
    fn a_waiting_edit_lives_until_done_or_withdrawn() {
        let mut h = Handoffs::default();
        let first = h.heard(HandoffEvent::Edit(edit(7, true)), true, NOW);
        assert!(matches!(first, Todo::Edit { again: false, .. }));
        assert_eq!(h.waiting_on("/r/.git/COMMIT_EDITMSG").map(|e| e.id), Some(7));
        let again = h.heard(HandoffEvent::Edit(edit(7, true)), true, NOW);
        assert!(matches!(again, Todo::Edit { again: true, .. }));
        assert_eq!(
            h.edited(7, EditOutcome::Done),
            Some(ClientMsg::Handoff(HandoffReply::Edited { id: 7, outcome: EditOutcome::Done }))
        );
        assert_eq!(h.edited(7, EditOutcome::Done), None, "once");

        h.heard(HandoffEvent::Edit(edit(8, true)), true, NOW);
        assert_eq!(h.heard(HandoffEvent::Withdrawn { id: 8 }, true, NOW), Todo::Withdraw { id: 8 });
        assert_eq!(h.edited(8, EditOutcome::Cancelled), None);

        assert_eq!(
            h.heard(HandoffEvent::Edit(edit(9, true)), false, NOW),
            Todo::Refuse { reply: ClientMsg::Handoff(HandoffReply::Refused { id: 9 }) }
        );
        h.heard(HandoffEvent::Edit(edit(10, false)), true, NOW);
        assert_eq!(h.edited(10, EditOutcome::Done), None, "nothing waits");
    }
}

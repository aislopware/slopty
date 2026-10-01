//! The clipboard as the sessions see it: the text a program may read (OSC 52), and what a paste
//! carries to a program that is told of pastes as Kitty paste events (mode 5522).
//!
//! A paste carries, beside its text, the paster's copy as rich text and pictures, but only
//! while that client shares its clipboard with the worker, only when the text is that copy's
//! (a paste of a selection or a snippet is just its text), and never a secret. File URLs stay
//! behind: they name files on the client, which a program here cannot open. Nothing here waits:
//! what is not here yet is asked of the client, and the session hears it arrive through
//! [`ForSessions::arrivals`].

use slopty_core::ClientId;
use slopty_input::pasteboard::Board;
use slopty_proto::terminal::MAX_OSC52_BYTES;
use slopty_proto::transfer::{ClipFormat, ClipMsg, Peer, RepRef, Source};
use tokio::sync::watch;

use super::{Access, Clipboard, Incoming, Key, Link, State, digest};

/// The formats a paste carries beside its text, as the program asks for them.
const CARRIED: [ClipFormat; 4] =
    [ClipFormat::Html, ClipFormat::Rtf, ClipFormat::Png, ClipFormat::Tiff];

/// What a paste carries of the paster's copy: which representations, of which offer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PastePlan {
    link: Link,
    source: Source,
    reps: Vec<(ClipFormat, Key)>,
    /// The most bytes the representations bring together.
    budget: usize,
}

impl PastePlan {
    /// The formats it carries, in the offer's order.
    pub fn formats(&self) -> impl Iterator<Item = ClipFormat> + '_ {
        self.reps.iter().map(|(format, _)| *format)
    }
}

/// The clipboard for the sessions' programs. Nothing in it waits or asks the person.
pub trait ForSessions: Send + Sync {
    /// The text a program may read now ([`Clipboard::shared_text_within`]), up to what one may
    /// copy.
    fn shared_text(&self) -> Option<String>;

    /// What a paste of `text` by `client` carries beside the text, at most `budget` bytes of
    /// it: `None` when it carries nothing.
    fn paste_plan(&self, client: ClientId, text: &str, budget: usize) -> Option<PastePlan>;

    /// The representations of `plan` still to come: not here, and neither refused nor gone.
    fn paste_missing(&self, plan: &PastePlan) -> Vec<RepRef>;

    /// Ask the client for `reps`, each once.
    fn paste_fetch(&self, plan: &PastePlan, reps: &[RepRef]);

    /// The representations of `plan` that are here, within its budget together.
    fn paste_take(&self, plan: &PastePlan) -> Vec<(ClipFormat, Vec<u8>)>;

    /// Moves whenever a client's bytes arrive, are refused or its offer goes.
    fn arrivals(&self) -> watch::Receiver<u64>;
}

impl<B: Board + Access + Send + Sync> ForSessions for Clipboard<B> {
    fn shared_text(&self) -> Option<String> {
        self.shared_text_within(MAX_OSC52_BYTES)
    }

    fn paste_plan(&self, client: ClientId, text: &str, budget: usize) -> Option<PastePlan> {
        let state = self.shared.state.lock();
        let (link, inc) = shared_offer(&state, client)?;
        if !carries(inc, text) {
            return None;
        }
        let cap = u64::try_from(budget).unwrap_or(u64::MAX);
        let reps: Vec<(ClipFormat, Key)> = CARRIED
            .into_iter()
            .filter_map(|format| {
                inc.offer
                    .reps()
                    .find(|(_, rep)| rep.kind.is(format) && rep.size.is_none_or(|s| s <= cap))
                    .map(|(n, rep)| (format, (n, rep.kind.clone())))
            })
            .collect();
        let plan = PastePlan { link, source: inc.offer.source(), reps, budget };
        drop(state);
        (!plan.reps.is_empty()).then_some(plan)
    }

    fn paste_missing(&self, plan: &PastePlan) -> Vec<RepRef> {
        let state = self.shared.state.lock();
        let Some(inc) = offer_of(&state, plan) else { return Vec::new() };
        if inc.gone {
            return Vec::new();
        }
        plan.reps
            .iter()
            .filter(|(_, key)| bytes_of(inc, key).is_none() && !inc.refused.contains(key))
            .map(|(_, (n, kind))| inc.offer.rep_ref(*n, kind.clone()))
            .collect()
    }

    fn paste_fetch(&self, plan: &PastePlan, reps: &[RepRef]) {
        let max = Some(u64::try_from(plan.budget).unwrap_or(u64::MAX));
        let mut state = self.shared.state.lock();
        let Some(sink) = state.sinks.get(&plan.link).cloned() else { return };
        let Some(inc) =
            state.incoming.get_mut(&plan.link).filter(|i| i.offer.source() == plan.source)
        else {
            return;
        };
        let asks: Vec<RepRef> = reps
            .iter()
            .filter(|rep| inc.asked.insert((rep.item, rep.kind.clone())))
            .cloned()
            .collect();
        drop(state);
        for rep in asks {
            sink(ClipMsg::Fetch { rep, max, urgent: true });
        }
    }

    fn paste_take(&self, plan: &PastePlan) -> Vec<(ClipFormat, Vec<u8>)> {
        let state = self.shared.state.lock();
        let Some(inc) = offer_of(&state, plan) else { return Vec::new() };
        let mut left = plan.budget;
        let mut taken = Vec::new();
        for (format, key) in &plan.reps {
            if let Some(bytes) = bytes_of(inc, key).filter(|b| b.len() <= left) {
                left = left.saturating_sub(bytes.len());
                taken.push((*format, bytes.to_vec()));
            }
        }
        drop(state);
        taken
    }

    fn arrivals(&self) -> watch::Receiver<u64> {
        self.shared.arrivals.subscribe()
    }
}

/// `client`'s offer, while it shares its clipboard and the offer is no secret.
fn shared_offer(state: &State, client: ClientId) -> Option<(Link, &Incoming)> {
    let (link, inc) =
        state.incoming.iter().find(|(_, inc)| inc.offer.origin == Peer::Client(client))?;
    (state.watchers.contains(link) && !inc.offer.concealed && !inc.gone).then_some((*link, inc))
}

/// The offer `plan` was made of, while it is still the client's.
fn offer_of<'s>(state: &'s State, plan: &PastePlan) -> Option<&'s Incoming> {
    state.incoming.get(&plan.link).filter(|inc| inc.offer.source() == plan.source)
}

/// Whether a paste of `text` is `inc`'s copy: its text, or nothing (a picture's paste chord).
fn carries(inc: &Incoming, text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    let hash = digest(text.as_bytes());
    inc.offer.reps().filter(|(_, rep)| rep.kind.is(ClipFormat::Text)).any(|(n, rep)| {
        rep.hash == Some(hash)
            || bytes_of(inc, &(n, rep.kind.clone())).is_some_and(|b| b == text.as_bytes())
    })
}

/// The bytes of `key` of `inc`'s offer, when they are here.
fn bytes_of<'i>(inc: &'i Incoming, key: &Key) -> Option<&'i [u8]> {
    inc.offer
        .rep(&inc.offer.rep_ref(key.0, key.1.clone()))
        .and_then(|rep| rep.inline.as_deref())
        .or_else(|| inc.fetched.get(key).map(Vec::as_slice))
}

#[cfg(test)]
mod tests;

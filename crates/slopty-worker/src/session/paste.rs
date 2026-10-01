//! A paste to a program told of pastes as Kitty paste events (mode 5522): the event lists the
//! paste's MIME types, and the program reads what it wants from the session at once, so every
//! representation listed is here before the event goes. The paster's copy as rich text and
//! pictures is fetched from the client first, for at most [`PASTE_WAIT`], and the viewers'
//! input waits behind it in order, as it waits behind a picture's paste.

use slopty_core::ClientId;
use slopty_engine::{EngineEvent, PasteRep};
use slopty_proto::terminal::{TermError, TermEvent};
use slopty_proto::transfer::ClipFormat;
use tokio::sync::watch;

use super::{Actor, Origin, engine_error};
use crate::clip::PastePlan;

/// How long a paste event waits for the paster's copy before it goes with what is here, as long
/// as the connection waits for a picture's paste.
const PASTE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// The most a paste carries beside its text. The program's read of it is answered on its input,
/// base64 and all, so this keeps the answer well inside what the input queue holds.
pub(super) const PASTE_CARRY_BYTES: usize = 8 << 20;

/// A paste event waiting for the representations it lists.
#[derive(Debug)]
pub(super) struct Pasting {
    client: ClientId,
    text: String,
    plan: PastePlan,
    until: tokio::time::Instant,
}

impl Pasting {
    pub(super) const fn until(&self) -> tokio::time::Instant {
        self.until
    }
}

/// Wake when the clients' clipboard bytes move; never without a clipboard.
pub(super) async fn arrived(arrivals: &mut Option<watch::Receiver<u64>>) {
    let Some(rx) = arrivals else { return std::future::pending().await };
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// The MIME type a program reads a format by.
const fn mime(format: ClipFormat) -> &'static str {
    match format {
        ClipFormat::Text => slopty_engine::TEXT_MIME,
        other => other.mime(),
    }
}

impl Actor {
    /// `client` pasted `text` into a program told of pastes as events: tell it now, or once
    /// the rest of the paster's copy is here.
    pub(super) fn paste_event(&mut self, client: ClientId, text: String) {
        let Some(clip) = self.clip.clone() else { return self.tell_paste(client, &text, None) };
        let Some(plan) = clip.paste_plan(client, &text, PASTE_CARRY_BYTES) else {
            return self.tell_paste(client, &text, None);
        };
        let missing = clip.paste_missing(&plan);
        if missing.is_empty() {
            return self.tell_paste(client, &text, Some(&plan));
        }
        clip.paste_fetch(&plan, &missing);
        let now = tokio::time::Instant::now();
        let until = now.checked_add(PASTE_WAIT).unwrap_or(now);
        self.pasting = Some(Pasting { client, text, plan, until });
    }

    /// The paster's bytes moved, or the wait is over (`due`): the event goes once nothing more
    /// is coming, and the input behind it follows.
    pub(super) fn paste_progress(&mut self, due: bool) {
        let Some(pasting) = &self.pasting else { return };
        let waiting =
            self.clip.as_ref().is_some_and(|c| !c.paste_missing(&pasting.plan).is_empty());
        if waiting && !due {
            return;
        }
        let Some(Pasting { client, text, plan, .. }) = self.pasting.take() else { return };
        self.tell_paste(client, &text, Some(&plan));
        while self.pasting.is_none() {
            let Some((client, req, at)) = self.held.pop_front() else { break };
            self.request(client, req, at);
        }
    }

    /// Tell the program of `client`'s paste of `text` with what of `plan` is here.
    fn tell_paste(&mut self, client: ClientId, text: &str, plan: Option<&PastePlan>) {
        let more: Vec<PasteRep> = plan
            .zip(self.clip.as_ref())
            .map(|(plan, clip)| clip.paste_take(plan))
            .unwrap_or_default()
            .into_iter()
            .map(|(format, data)| PasteRep { mime: mime(format), data })
            .collect();
        // Whatever the engine had to say before the paste is said first.
        self.after_output();
        if let Err(e) = self.engine.paste_event(text, more) {
            return self.send_to(client, &engine_error(&e));
        }
        let mut refused: Option<TermError> = None;
        for ev in self.engine.drain_events() {
            if let EngineEvent::PtyWrite(bytes) = ev
                && let Err(e) = self.queue_input(&bytes, Origin::Viewer { key: None })
            {
                refused = Some(e);
            }
        }
        if let Some(e) = refused {
            self.send_to(client, &TermEvent::Error(e));
        }
    }
}

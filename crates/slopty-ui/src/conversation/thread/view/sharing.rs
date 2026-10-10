//! The composer's words, followed across devices: what the person writes to a thread and has not
//! sent goes to its worker as the thread's draft ([`Intent::Draft`]) once they pause, and a
//! composer left empty here takes up a draft another device kept since. Whoever wrote last
//! holds the draft; an empty composer after a send clears it, leaving an empty draft of its
//! time, and a composer elsewhere still holding what it last shared or took up, untouched since,
//! empties on it: a message sent on one device leaves every composer. Drafts go only while the
//! worker is linked, never through the outbox, so words held while away cannot land over a
//! draft kept since. The words are also kept on this device, in the drafts file
//! (`workspace::drafts`), which covers a draft too long for the row.

use std::time::Duration;

use gpui::{Context, Task, Window};
use slopty_core::WallMs;
use slopty_proto::thread::wire::{Draft, Intent};

use super::ThreadView;

/// How long after the last change the words go to the worker: a pause in the writing, not every
/// key, since each one is a row change every device hears.
pub(crate) const DRAFT_SHARE_PAUSE: Duration = Duration::from_secs(2);

/// What this view and the thread's worker agree the draft is.
#[derive(Default)]
pub(super) struct Sharing {
    /// The words the worker holds as far as this view knows: what it last sent or took up.
    /// `None` until either happened, when the row's own draft says.
    shared: Option<String>,
    /// The time of the newest worker draft this view has heard, so each is taken up once.
    heard: Option<WallMs>,
    /// The words this device kept and the view opened with, and when they were kept, while
    /// the composer still holds them untouched: a worker draft kept later stands over them.
    restored: Option<(String, WallMs)>,
    /// The pause before the words go.
    pause: Option<Task<()>>,
}

impl std::fmt::Debug for Sharing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sharing")
            .field("shared", &self.shared.as_ref().map(String::len))
            .field("heard", &self.heard)
            .finish_non_exhaustive()
    }
}

impl ThreadView {
    /// The thread's draft as its worker's row has it.
    fn row_draft(&self, cx: &gpui::App) -> Option<Draft> {
        self.hub.read(cx).threads().rows().rows.get(&self.thread).and_then(|r| r.draft.clone())
    }

    /// Open the composer with words this device kept at `kept`: a worker draft kept since,
    /// heard once the table comes, takes their place while they are untouched.
    pub fn restore_kept_draft(
        &mut self,
        text: &str,
        kept: Option<WallMs>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sharing.restored = kept.map(|kept| (text.to_owned(), kept));
        self.restore_draft(text, window, cx);
    }

    /// The words changed: they go to the worker once the person has paused
    /// [`DRAFT_SHARE_PAUSE`]. Not for a thread not started yet, which no worker holds.
    pub(super) fn draft_changed(&mut self, cx: &Context<Self>) {
        if self.draft.is_some() {
            return;
        }
        self.sharing.pause = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_SHARE_PAUSE).await;
            let _gone = this.update(cx, |this, cx| {
                this.sharing.pause = None;
                this.share_draft(cx);
            });
        }));
    }

    /// Send the words now if they are not what the worker holds: as the pause ends, and as the
    /// view goes or the app leaves the front. Words past [`Draft::MAX_BYTES`] stay on this
    /// device.
    pub fn share_draft(&mut self, cx: &mut Context<Self>) {
        self.sharing.pause = None;
        let Some(text) = self.unshared(cx) else { return };
        self.sharing.shared = Some(text.clone());
        let thread = self.thread;
        self.hub.update(cx, |hub, cx| {
            let _id = hub.intent(thread, Intent::Draft { text }, cx);
        });
    }

    /// The words, when they are not what the worker holds and may go to it. Nothing goes
    /// while the link is down, since an intent kept for later could land over a draft kept
    /// since on another device; until this view has heard the worker's draft, nothing goes
    /// while the row is unknown either.
    pub(super) fn unshared(&self, cx: &gpui::App) -> Option<String> {
        if self.draft.is_some() {
            return None;
        }
        let hub = self.hub.read(cx);
        if !hub.linked() {
            return None;
        }
        let text = self.draft(cx);
        let held = if let Some(shared) = &self.sharing.shared {
            shared.clone()
        } else {
            let row = hub.threads().rows().rows.get(&self.thread)?;
            row.draft.as_ref().map(|d| d.text.clone()).unwrap_or_default()
        };
        (text.trim() != held.trim() && text.len() <= Draft::MAX_BYTES).then_some(text)
    }

    /// The worker's table moved: a draft newer than any heard, kept by another device, is
    /// taken up while the composer is empty, or still holds words this device kept before it.
    /// Words cleared there (sent or wiped) clear a composer still holding what this view last
    /// shared or took up.
    /// One this view sent itself is never taken back, so a message sent before its own draft
    /// came back does not return. Words held back until the table came go now.
    pub(super) fn hear_row_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft.is_some() {
            return;
        }
        self.take_row_draft(window, cx);
        if self.sharing.pause.is_none() && self.unshared(cx).is_some() {
            self.draft_changed(cx);
        }
    }

    fn take_row_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.row_draft(cx) else { return };
        if self.sharing.heard.is_some_and(|heard| heard >= draft.at_ms) {
            return;
        }
        self.sharing.heard = Some(draft.at_ms);
        let text = self.draft(cx);
        let mine = self.sharing.shared.as_deref() == Some(draft.text.as_str());
        let stale = self
            .sharing
            .restored
            .as_ref()
            .is_some_and(|(words, kept)| *words == text && draft.at_ms > *kept);
        // An empty draft is words cleared on another device; what this view last agreed with
        // the worker, still untouched here, went with them.
        let cleared = draft.text.is_empty()
            && self.sharing.shared.as_deref().is_some_and(|shared| shared.trim() == text.trim());
        if mine || self.composing.editing() || (!text.trim().is_empty() && !stale && !cleared) {
            if self.sharing.shared.is_none() {
                self.sharing.shared = Some(draft.text);
            }
            return;
        }
        self.sharing.shared = Some(draft.text.clone());
        self.sharing.restored = None;
        self.restore_draft(&draft.text, window, cx);
    }
}

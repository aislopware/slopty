//! The worker's pasteboard, looked at only while a client wants it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use slopty_proto::WorkerMsg;
use slopty_proto::transfer::ClipMsg;
use slopty_worker::clip::Interest;

use crate::Daemon;

/// How often the pasteboard is looked at while a client watches it: one Mach call to the
/// pasteboard server for the change count, and a read only when it moved.
const WATCHED: Duration = Duration::from_millis(50);

/// How often the change count alone is read while clients are linked but none watches, so a
/// copy made on the worker meanwhile is known to be newer than theirs.
const LINKED: Duration = Duration::from_millis(250);

/// Announce every change of the worker pasteboard while some client watches it, note changes
/// while clients are only linked, and sleep, reading nothing, while none is. Each look also
/// clears a secret a client pasted once its time is up. `conn` forwards an offer only to the
/// clients that watch.
pub async fn watch(daemon: Daemon) {
    let mut interest = daemon.clip.interest();
    loop {
        let now = *interest.borrow_and_update();
        let period = match now {
            Interest::Idle => {
                if interest.changed().await.is_err() {
                    return;
                }
                continue;
            }
            Interest::Linked => LINKED,
            Interest::Watched => WATCHED,
        };
        tokio::select! {
            () = tokio::time::sleep(period) => {}
            changed = interest.changed() => {
                if changed.is_err() {
                    return;
                }
                continue;
            }
        }
        let clip = Arc::clone(&daemon.clip);
        // Off the runtime: the pasteboard server answers in its own time.
        let looked = tokio::task::spawn_blocking(move || {
            let at = Instant::now();
            clip.expire(at);
            if now == Interest::Watched {
                clip.poll(at)
            } else {
                clip.observe(at);
                None
            }
        });
        if let Ok(Some(offer)) = looked.await {
            tracing::debug!(
                generation = offer.generation,
                items = offer.items.len(),
                concealed = offer.concealed,
                "worker clipboard changed"
            );
            let _sent = daemon.events.send(WorkerMsg::Clip(ClipMsg::Offer(offer)));
        }
    }
}

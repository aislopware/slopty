//! The worker's pasteboard, watched only while a client wants it.

use std::sync::Arc;
use std::time::Duration;

use slopty_proto::WorkerMsg;
use slopty_proto::transfer::ClipMsg;

use crate::Daemon;

/// How often the pasteboard's change count is read while watched (one Mach call to the
/// pasteboard server).
const PERIOD: Duration = Duration::from_millis(200);

/// Announce every change of the worker pasteboard while some client watches it; sleep, reading
/// nothing, while none does. `conn` forwards an offer only to the clients that watch.
pub async fn watch(daemon: Daemon) {
    let mut watched = daemon.clip.watched();
    let mut tick = tokio::time::interval(PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if !*watched.borrow_and_update() {
            if watched.changed().await.is_err() {
                return;
            }
            continue;
        }
        tokio::select! {
            _ = tick.tick() => {}
            changed = watched.changed() => {
                if changed.is_err() {
                    return;
                }
                continue;
            }
        }
        let clip = Arc::clone(&daemon.clip);
        // Off the runtime: reading a picture off the pasteboard server takes milliseconds.
        if let Ok(Some(offer)) = tokio::task::spawn_blocking(move || clip.poll()).await {
            tracing::debug!(
                generation = offer.generation,
                items = offer.items.len(),
                "worker clipboard changed"
            );
            let _sent = daemon.events.send(WorkerMsg::Clip(ClipMsg::Offer(offer)));
        }
    }
}

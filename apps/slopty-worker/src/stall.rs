//! The self-test's stalled control stream (`SLOPTY_E2E_STALL_CONTROL`), in the `e2e` build only:
//! while the file it names exists, no control stream of this worker carries, a client's or the
//! server's, new ones included, while everything else (screen datagrams, terminals' streams,
//! QUIC's keep-alive) goes on. It stands for a reader or writer that stopped under a
//! connection that still looks alive, which only the links' pings can tell
//! (`slopty_net::liveness`). No release build carries the seam: there, [`hold`] returns at once.

/// The variable naming the file whose presence stalls the control streams.
#[cfg(feature = "e2e")]
pub const ENV: &str = "SLOPTY_E2E_STALL_CONTROL";

/// Wait while the control streams are stalled; at once in a build without the seam.
#[cfg(not(feature = "e2e"))]
#[expect(clippy::unused_async, reason = "the e2e build's holds; this one is the same call")]
pub async fn hold() {}

/// Wait while the control streams are stalled: while the file [`ENV`] names exists.
#[cfg(feature = "e2e")]
pub async fn hold() {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    /// How often the file is looked for, and how soon a stall lifts.
    const LOOK: std::time::Duration = std::time::Duration::from_millis(20);
    static STALLED: AtomicBool = AtomicBool::new(false);
    static WATCHING: OnceLock<()> = OnceLock::new();
    WATCHING.get_or_init(|| {
        let Some(path) = std::env::var_os(ENV).map(std::path::PathBuf::from) else { return };
        tracing::warn!(path = %path.display(), "the control streams stall while this file exists");
        tokio::spawn(async move {
            let mut was = false;
            loop {
                let now = path.exists();
                if now != was {
                    tracing::warn!(stalled = now, "control streams");
                    was = now;
                }
                STALLED.store(now, Ordering::SeqCst);
                tokio::time::sleep(LOOK).await;
            }
        });
    });
    while STALLED.load(Ordering::SeqCst) {
        tokio::time::sleep(LOOK).await;
    }
}

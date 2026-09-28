//! What the UI asks of a worker beyond the control stream: files up and down, the bytes of a
//! clipboard offer, and a port of the worker served here. Behind a trait so the UI's tests can
//! record the calls.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use slopty_core::{WallMs, XferId};
use slopty_net::{ClientMsg, Connection};
use slopty_proto::transfer::{BulkHeader, ClipFormat, ClipMsg, Dest, Purpose, XferMsg};
use tokio::sync::mpsc;

use crate::LinkEvent;
use crate::clip::{ClipCache, fits_inline};
use crate::tunnel::Forwards;
use crate::xfer::{self, Uplink, XferError};

/// Files and clipboard bytes to and from one worker.
pub trait Remote: Send + Sync + std::fmt::Debug {
    /// Send `files` (files or directories here) to `dest` on the worker as transfer `xfer`.
    /// Returns at once; the worker's `Progress`, `Done` and `Finished` arrive on the link, and a
    /// failure here as [`LinkEvent::XferFailed`].
    fn upload(&self, xfer: XferId, files: Vec<PathBuf>, dest: Dest);

    /// Stop transfer `xfer`; what the worker holds of it stays for a resume.
    fn cancel(&self, xfer: XferId);

    /// Bring the worker's `path` (a file, or a directory as its files) into the directory
    /// `into`, blocking until every file has landed. Never call it on the main thread.
    fn download(&self, path: String, into: PathBuf) -> Result<Vec<PathBuf>, XferError>;

    /// The bytes in `format` of the worker's clipboard offer `generation`, waiting at most
    /// `wait` for them. Blocks: this is what a pasteboard's data provider calls.
    fn clip_data(&self, generation: u64, format: ClipFormat, wait: Duration) -> Option<Vec<u8>>;

    /// Answer the worker's fetch of this client's offer `generation`: inline when it fits,
    /// else on a bulk stream.
    fn send_clip(&self, generation: u64, format: ClipFormat, bytes: Vec<u8>);

    /// Serve the worker's loopback `port` on this machine, until the link goes, and say
    /// where: the same port when it is free here, else the next free one. `None` on a link
    /// that forwards nothing, or when no local port could be had.
    fn forward(&self, port: u16) -> Option<u16>;
}

/// The [`Remote`] of a live link.
#[derive(Debug)]
pub struct LinkRemote {
    up: Uplink,
    clips: Arc<ClipCache>,
    events: mpsc::Sender<LinkEvent>,
    runtime: tokio::runtime::Handle,
    /// The link's port forwards, shared with its control reader; `None` when it forwards none.
    forwards: Option<Arc<Mutex<Forwards>>>,
}

impl LinkRemote {
    /// The remote of a link: its uplink and clipboard cache, where its failures are reported,
    /// the runtime its tasks run on, and its port forwards if it has any.
    #[must_use]
    pub const fn new(
        up: Uplink,
        clips: Arc<ClipCache>,
        events: mpsc::Sender<LinkEvent>,
        runtime: tokio::runtime::Handle,
        forwards: Option<Arc<Mutex<Forwards>>>,
    ) -> Self {
        Self { up, clips, events, runtime, forwards }
    }

    const fn conn(&self) -> &Connection {
        &self.up.conn
    }
}

impl Remote for LinkRemote {
    fn upload(&self, xfer: XferId, files: Vec<PathBuf>, dest: Dest) {
        let up = self.up.clone();
        let events = self.events.clone();
        self.runtime.spawn(async move {
            if let Err(error) = xfer::upload(&up, xfer, &files, dest).await {
                let _gone = events.send(LinkEvent::XferFailed { xfer, error }).await;
            }
        });
    }

    fn cancel(&self, xfer: XferId) {
        self.up.table.cancel(xfer);
        if let Err(e) = self.up.out.try_send(ClientMsg::Xfer(XferMsg::Cancel { xfer })) {
            tracing::debug!(%xfer, error = %e, "cancel not sent");
        }
    }

    fn download(&self, path: String, into: PathBuf) -> Result<Vec<PathBuf>, XferError> {
        let up = self.up.clone();
        let conn = self.conn().clone();
        let xfer = XferId::new();
        self.runtime.block_on(async move {
            tokio::select! {
                landed = xfer::download(&up, xfer, path, into) => landed,
                () = async { conn.closed().await; } => Err(XferError::LinkClosed),
            }
        })
    }

    fn clip_data(&self, generation: u64, format: ClipFormat, wait: Duration) -> Option<Vec<u8>> {
        self.clips.wait(generation, format, wait)
    }

    fn send_clip(&self, generation: u64, format: ClipFormat, bytes: Vec<u8>) {
        if fits_inline(bytes.len()) {
            let data = ClientMsg::Clip(ClipMsg::Data { generation, format, bytes });
            if let Err(e) = self.up.out.try_send(data) {
                tracing::debug!(error = %e, "clipboard data not sent");
            }
            return;
        }
        let conn = self.conn().clone();
        self.runtime.spawn(async move {
            let header = BulkHeader {
                xfer: XferId::new(),
                purpose: Purpose::Clip { generation, format },
                name: String::new(),
                size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                mtime_ms: WallMs::ZERO,
                mode: 0o600,
                offset: 0,
            };
            let sent = async {
                let mut send = slopty_net::streams::open_bulk(&conn, header)
                    .await
                    .map_err(|e| e.to_string())?;
                send.write_all(&bytes).await.map_err(|e| e.to_string())?;
                send.finish().map_err(|e| e.to_string())
            };
            if let Err(e) = sent.await {
                tracing::debug!(generation, error = %e, "clipboard bulk");
            }
        });
    }

    fn forward(&self, port: u16) -> Option<u16> {
        let forwards = self.forwards.as_ref()?;
        // A listener is a tokio socket and its accept loop a tokio task: both need the runtime.
        let _runtime = self.runtime.enter();
        forwards.lock().pin(port)
    }
}

//! What the UI asks of a worker beyond the control stream: files up and down, the bytes of a
//! clipboard offer, and the worker's ports and network served here.
//!
//! Behind a trait so the UI's tests can record the calls.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use slopty_core::{WallMs, XferId};
use slopty_net::{ClientMsg, Connection};
use slopty_proto::transfer::{BulkHeader, ClipMsg, Dest, Purpose, RepRef, TunnelRefusal, XferMsg};
use tokio::sync::mpsc;

use crate::LinkEvent;
use crate::clip::{ClipCache, Fetched, fits_inline};
#[cfg(target_vendor = "apple")]
use crate::dnd::out::{DragOuts, Shared};
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

    /// Representation `rep` of the worker's clipboard offer, capped at `max`, waiting at most
    /// `wait` for it. Blocks: this is what a pasteboard's data provider calls.
    fn clip_fetch(&self, rep: &RepRef, max: Option<u64>, wait: Duration) -> Fetched;

    /// Answer the worker's fetch of `rep`: bytes inline when they fit, else on a bulk stream,
    /// ahead of background transfers when a paste on the worker waits on it (`urgent`).
    fn send_clip(&self, rep: RepRef, answer: Fetched, urgent: bool);

    /// Serve the worker's loopback `port` on this machine, until the link goes, and say
    /// where: the same port when it is free here, else the next free one. `None` on a link
    /// that forwards nothing, or when no local port could be had.
    fn forward(&self, port: u16) -> Option<u16>;

    /// Serve every other host the worker reaches here, until the link goes, and say where: a
    /// SOCKS5 proxy's port on this machine's loopback ([`crate::tunnel::Proxy`]). `None` on
    /// a link that serves nothing, or when no local port could be had.
    fn proxy(&self) -> Option<u16> {
        None
    }

    /// Why the worker could not reach `host:port` the last time a page asked through
    /// [`Self::proxy`], unless it has answered since.
    fn refusal(&self, _host: &str, _port: u16) -> Option<TunnelRefusal> {
        None
    }

    /// Hand the worker's word on `shared`'s drag out to it as the link reads it, off the main
    /// thread, where a target's read of its data waits ([`crate::dnd::out::DragOuts`]). A
    /// remote with no link hears nothing.
    #[cfg(target_vendor = "apple")]
    fn watch_drag_out(&self, _shared: &Arc<Shared>) {}
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
    /// The drags out its control reader hands the worker's word to.
    #[cfg(target_vendor = "apple")]
    drag_outs: Arc<DragOuts>,
}

impl LinkRemote {
    /// The remote of a link: its uplink and clipboard cache, where its failures are reported,
    /// the runtime its tasks run on, its port forwards if it has any, and the drags out its
    /// control reader tells.
    #[must_use]
    pub const fn new(
        up: Uplink,
        clips: Arc<ClipCache>,
        events: mpsc::Sender<LinkEvent>,
        runtime: tokio::runtime::Handle,
        forwards: Option<Arc<Mutex<Forwards>>>,
        #[cfg(target_vendor = "apple")] drag_outs: Arc<DragOuts>,
    ) -> Self {
        Self {
            up,
            clips,
            events,
            runtime,
            forwards,
            #[cfg(target_vendor = "apple")]
            drag_outs,
        }
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

    fn clip_fetch(&self, rep: &RepRef, max: Option<u64>, wait: Duration) -> Fetched {
        self.clips.fetch(rep, max, wait)
    }

    fn send_clip(&self, rep: RepRef, answer: Fetched, urgent: bool) {
        let bytes = match answer {
            Fetched::Data(bytes) if !fits_inline(bytes.len()) => bytes,
            answer => {
                let msg = match answer {
                    Fetched::Data(bytes) => ClipMsg::Data { rep, bytes },
                    Fetched::TooBig(size) => ClipMsg::TooBig { rep, size },
                    Fetched::Gone => ClipMsg::Unavailable { source: rep.source },
                };
                if let Err(e) = self.up.out.try_send(ClientMsg::Clip(msg)) {
                    tracing::debug!(error = %e, "clipboard answer not sent");
                }
                return;
            }
        };
        let conn = self.conn().clone();
        self.runtime.spawn(async move {
            let item = rep.item;
            let header = BulkHeader {
                xfer: XferId::new(),
                purpose: Purpose::Rep { rep },
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
                if urgent {
                    // A paste waits on it: level with the tunnels, ahead of files.
                    send.set_priority(slopty_net::streams::TUNNEL_PRIORITY)
                        .map_err(|e| e.to_string())?;
                }
                send.write_all(&bytes).await.map_err(|e| e.to_string())?;
                send.finish().map_err(|e| e.to_string())
            };
            if let Err(e) = sent.await {
                tracing::debug!(item, error = %e, "clipboard bulk");
            }
        });
    }

    fn forward(&self, port: u16) -> Option<u16> {
        let forwards = self.forwards.as_ref()?;
        // A listener is a tokio socket and its accept loop a tokio task: both need the runtime.
        let _runtime = self.runtime.enter();
        forwards.lock().pin(port)
    }

    fn proxy(&self) -> Option<u16> {
        let forwards = self.forwards.as_ref()?;
        let _runtime = self.runtime.enter();
        forwards.lock().proxy().map(crate::tunnel::Proxy::local)
    }

    fn refusal(&self, host: &str, port: u16) -> Option<TunnelRefusal> {
        let forwards = self.forwards.as_ref()?;
        forwards.lock().refusal(host, port)
    }

    #[cfg(target_vendor = "apple")]
    fn watch_drag_out(&self, shared: &Arc<Shared>) {
        self.drag_outs.watch(shared);
    }
}

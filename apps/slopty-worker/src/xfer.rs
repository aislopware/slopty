//! The unidirectional streams a client opens (files and clipboard data coming up) and the
//! files it fetches (going down).

use std::time::{Duration, Instant};

use bytes::Bytes;
use slopty_core::{ClientId, XferId};
use slopty_net::streams::{self, RawRecv, Uni};
use slopty_net::{Connection, NetError, WorkerMsg};
use slopty_proto::transfer::{BulkHeader, INLINE_CLIP_BYTES, Purpose, XferMsg};
use slopty_worker::clip::MAX_REP_BYTES;
use slopty_worker::xfer::{Landed, Receiving, XferError, outgoing};
use tokio::io::AsyncReadExt as _;
use tokio::sync::mpsc;

use crate::Daemon;

/// Bytes read from a stream or a file at a time.
const CHUNK: usize = 256 << 10;
/// Chunks queued between a stream and the thread writing its file.
const QUEUED: usize = 16;
/// How long a file's stream waits for its transfer's `Begin`, which rides another stream.
const BEGIN_WAIT: Duration = Duration::from_secs(5);

/// Clipboard bytes a client sent up as a bulk stream, for the connection's loop.
#[derive(Debug)]
pub struct ClipData {
    /// The client's offer.
    pub generation: u64,
    /// Which representation.
    pub uti: String,
    /// The bytes.
    pub bytes: Vec<u8>,
}

/// Accept the client's unidirectional streams until the connection ends: files are written
/// where their transfer says, clipboard data goes to the connection's loop on `clips`.
pub async fn accept(
    daemon: Daemon,
    conn: Connection,
    client: ClientId,
    out: mpsc::Sender<WorkerMsg>,
    clips: mpsc::Sender<ClipData>,
) {
    loop {
        let uni = match streams::accept_uni(&conn).await {
            Ok(uni) => uni,
            Err(e) => {
                if conn.close_reason().is_some() {
                    break;
                }
                tracing::debug!(%client, error = %e, "unidirectional stream refused");
                continue;
            }
        };
        match uni {
            Uni::Session { session, .. } => {
                tracing::debug!(%client, %session, "a client opened a session stream; ignored");
            }
            Uni::Bulk { header, mut rx } => match header.purpose.clone() {
                Purpose::Upload => {
                    drop(tokio::spawn(receive(daemon.clone(), header, rx, out.clone())));
                }
                Purpose::Clip { generation, uti } => {
                    let clips = clips.clone();
                    drop(tokio::spawn(async move {
                        if let Some(bytes) = read_clip(&header, &mut rx).await {
                            let _sent = clips.send(ClipData { generation, uti, bytes }).await;
                        }
                    }));
                }
                Purpose::Download => {
                    tracing::debug!(%client, "a download sent up; refused");
                    rx.stop();
                }
            },
        }
    }
}

/// A clipboard representation's bytes, when the stream carries all it announced and no more
/// than [`MAX_REP_BYTES`].
async fn read_clip(header: &BulkHeader, rx: &mut RawRecv) -> Option<Vec<u8>> {
    if header.size > MAX_REP_BYTES {
        rx.stop();
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(header.size).ok()?);
    while let Some(chunk) = rx.chunk(CHUNK).await.ok()? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 > header.size {
            rx.stop();
            return None;
        }
    }
    (bytes.len() as u64 == header.size).then_some(bytes)
}

/// Receive one file of an upload and report it: `Done` once it is in place, then `Finished`
/// when it was the transfer's last; `Failed` when it could not land (its partial stays for a
/// resume).
async fn receive(
    daemon: Daemon,
    header: BulkHeader,
    mut rx: RawRecv,
    out: mpsc::Sender<WorkerMsg>,
) {
    let xfer = header.xfer;
    let name = header.name.clone();
    let landed = match write(&daemon, &header, &mut rx, &out).await {
        Ok(Some(landed)) => landed,
        Ok(None) => {
            tracing::debug!(%xfer, %name, "upload cancelled");
            return;
        }
        Err(e) => {
            tracing::info!(%xfer, %name, error = %e, "upload failed");
            let failed = XferMsg::Failed { xfer, name: Some(name), error: e.to_string() };
            let _sent = out.send(WorkerMsg::Xfer(failed)).await;
            return;
        }
    };
    tracing::info!(%xfer, %name, path = %landed.path.display(), bytes = landed.size, "file landed");
    let path = landed.path.to_string_lossy().into_owned();
    let done = XferMsg::Done { xfer, name: name.clone(), path, hash: landed.hash };
    let _sent = out.send(WorkerMsg::Xfer(done)).await;
    let Some(finished) = daemon.transfers.landed(xfer, &name, landed) else { return };
    if finished.staging {
        let clip = std::sync::Arc::clone(&daemon.clip);
        let paths = finished.paths.clone();
        let _written = tokio::task::spawn_blocking(move || clip.write_files(&paths)).await;
    }
    let paths = finished.paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let _sent = out.send(WorkerMsg::Xfer(XferMsg::Finished { xfer, paths })).await;
}

/// Write the stream into its file on a blocking thread, reporting progress. `Ok(None)` when
/// the transfer was cancelled.
async fn write(
    daemon: &Daemon,
    header: &BulkHeader,
    rx: &mut RawRecv,
    out: &mpsc::Sender<WorkerMsg>,
) -> Result<Option<Landed>, XferError> {
    let xfer = header.xfer;
    if !daemon.transfers.begun(xfer, BEGIN_WAIT).await {
        rx.stop();
        return Err(XferError::Unknown);
    }
    let target = daemon.transfers.target(xfer, &header.name).inspect_err(|_e| rx.stop())?;
    let mut cancel = daemon.transfers.cancelled(xfer).ok_or(XferError::Unknown)?;
    let (tx, mut chunks) = mpsc::channel::<Bytes>(QUEUED);
    let (offset, size, mode, mtime) = (header.offset, header.size, header.mode, header.mtime_ms);
    let writer = tokio::task::spawn_blocking(move || {
        let mut file = Receiving::open(&target, offset, size)?;
        while let Some(chunk) = chunks.blocking_recv() {
            if let Err(e) = file.write(&chunk) {
                let _held = file.keep();
                return Err(e);
            }
        }
        if file.at() == size {
            file.finish(mode, mtime)
        } else {
            let got = file.keep();
            Err(XferError::Incomplete { got, size })
        }
    });
    let mut cancelled = false;
    let mut ended = false;
    loop {
        tokio::select! {
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow_and_update() {
                    cancelled = true;
                    break;
                }
            }
            chunk = rx.chunk(CHUNK) => match chunk {
                Ok(Some(bytes)) => {
                    let n = bytes.len() as u64;
                    if tx.send(bytes).await.is_err() {
                        // The writer failed; its error says why.
                        break;
                    }
                    if let Some(done) = daemon.transfers.progress(xfer, n, Instant::now()) {
                        let _sent = out.try_send(WorkerMsg::Xfer(XferMsg::Progress { xfer, done }));
                    }
                }
                Ok(None) => {
                    ended = true;
                    break;
                }
                Err(e) => {
                    tracing::debug!(%xfer, name = %header.name, error = %e, "upload stream cut");
                    break;
                }
            },
        }
    }
    if !ended {
        rx.stop();
    }
    drop(tx);
    let written = writer.await.map_err(|e| XferError::Io(std::io::Error::other(e)))?;
    match written {
        Err(_e) if cancelled => Ok(None),
        other => other.map(Some),
    }
}

/// Send the file or directory at `path` down as transfer `xfer`: a `Begin`, then one bulk
/// stream per file. Failures are reported as `Failed`.
pub async fn download(conn: Connection, out: mpsc::Sender<WorkerMsg>, xfer: XferId, path: String) {
    let at = slopty_worker::file::expand_home(std::path::Path::new(&path));
    let listed = tokio::task::spawn_blocking(move || outgoing(&at)).await;
    let files = match listed {
        Ok(Ok(files)) => files,
        Ok(Err(e)) => return fail(&out, xfer, None, &e.to_string()).await,
        Err(e) => return fail(&out, xfer, None, &e.to_string()).await,
    };
    let bytes = files.iter().map(|f| f.size).sum();
    let count = u32::try_from(files.len()).unwrap_or(u32::MAX);
    tracing::info!(%xfer, %path, files = count, bytes, "download");
    let begin = XferMsg::Begin { xfer, dest: None, files: count, bytes };
    if out.send(WorkerMsg::Xfer(begin)).await.is_err() {
        return;
    }
    for file in files {
        let header = BulkHeader {
            xfer,
            purpose: Purpose::Download,
            name: file.name.clone(),
            size: file.size,
            mtime_ms: file.mtime_ms,
            mode: file.mode,
            offset: 0,
        };
        if let Err(e) = send_file(&conn, header, &file.path).await {
            tracing::info!(%xfer, name = %file.name, error = %e, "download failed");
            return fail(&out, xfer, Some(file.name), &e.to_string()).await;
        }
    }
}

async fn fail(out: &mpsc::Sender<WorkerMsg>, xfer: XferId, name: Option<String>, error: &str) {
    let failed = XferMsg::Failed { xfer, name, error: error.to_owned() };
    let _sent = out.send(WorkerMsg::Xfer(failed)).await;
}

/// One file down a bulk stream: exactly the bytes its header announces.
async fn send_file(
    conn: &Connection,
    header: BulkHeader,
    path: &std::path::Path,
) -> Result<(), NetError> {
    let size = header.size;
    let mut file =
        tokio::fs::File::open(path).await.map_err(|e| NetError::Stream(e.to_string()))?;
    let mut send = streams::open_bulk(conn, header).await?;
    let mut buf = vec![0_u8; CHUNK];
    let mut sent = 0_u64;
    while sent < size {
        let n = file.read(&mut buf).await.map_err(|e| NetError::Stream(e.to_string()))?;
        if n == 0 {
            break;
        }
        let n = usize::try_from((n as u64).min(size.saturating_sub(sent))).unwrap_or(n);
        send.write_all(buf.get(..n).unwrap_or_default())
            .await
            .map_err(|e| NetError::Stream(e.to_string()))?;
        sent = sent.saturating_add(n as u64);
    }
    send.finish().map_err(|e| NetError::Stream(e.to_string()))
}

/// Answer a client's fetch of the worker's clipboard: inline when it fits, a bulk stream when
/// not, `Unavailable` when the pasteboard changed since the offer.
pub async fn send_clip(
    daemon: &Daemon,
    conn: &Connection,
    out: &mpsc::Sender<WorkerMsg>,
    generation: u64,
    uti: String,
) {
    use slopty_proto::transfer::ClipMsg;
    let Some(bytes) = daemon.clip.fetch(generation, &uti) else {
        let _sent = out.send(WorkerMsg::Clip(ClipMsg::Unavailable { generation })).await;
        return;
    };
    if bytes.len() <= INLINE_CLIP_BYTES {
        let data = ClipMsg::Data { generation, uti, bytes: bytes.to_vec() };
        let _sent = out.send(WorkerMsg::Clip(data)).await;
        return;
    }
    let header = BulkHeader {
        xfer: XferId::new(),
        purpose: Purpose::Clip { generation, uti },
        name: String::new(),
        size: bytes.len() as u64,
        mtime_ms: 0,
        mode: 0,
        offset: 0,
    };
    let conn = conn.clone();
    drop(tokio::spawn(async move {
        let sent = async {
            let mut send = streams::open_bulk(&conn, header).await?;
            send.write_all(&bytes).await.map_err(|e| NetError::Stream(e.to_string()))?;
            send.finish().map_err(|e| NetError::Stream(e.to_string()))
        };
        if let Err(e) = sent.await {
            tracing::debug!(generation, error = %e, "clipboard data not sent");
        }
    }));
}

//! The unidirectional streams a client opens (files and clipboard data coming up) and the
//! files it fetches (going down).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use slopty_core::{ClientId, WallMs, XferId};
use slopty_net::streams::{self, RawRecv, Uni};
use slopty_net::{Connection, NetError, WorkerMsg};
use slopty_proto::file::{FILE_BYTES, WriteResult};
use slopty_proto::transfer::{BulkHeader, ClipFormat, Hash, INLINE_CLIP_BYTES, Purpose, XferMsg};
use slopty_worker::clip::MAX_REP_BYTES;
use slopty_worker::xfer::{Landed, Receiving, Transfers, XferError, outgoing};
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
    /// In which format.
    pub format: ClipFormat,
    /// The bytes.
    pub bytes: Vec<u8>,
}

/// Accept the client's unidirectional streams until the connection ends: files are written
/// where their transfer says, clipboard data goes to the connection's loop on `clips`. Each
/// header is read on the stream's own task: one whose header was lost holds up no stream opened
/// after it.
pub async fn accept(
    daemon: Daemon,
    conn: Connection,
    client: ClientId,
    out: mpsc::Sender<WorkerMsg>,
    clips: mpsc::Sender<ClipData>,
) {
    loop {
        let recv = match conn.accept_uni().await {
            Ok(recv) => recv,
            Err(e) => {
                tracing::debug!(%client, error = %e, "unidirectional streams end");
                break;
            }
        };
        let (daemon, out, clips) = (daemon.clone(), out.clone(), clips.clone());
        drop(tokio::spawn(async move {
            match streams::read_uni(recv).await {
                Ok(uni) => take(daemon, client, uni, out, clips).await,
                Err(e) => tracing::debug!(%client, error = %e, "unidirectional stream refused"),
            }
        }));
    }
}

/// Serve one unidirectional stream whose header has been read.
async fn take(
    daemon: Daemon,
    client: ClientId,
    uni: Uni,
    out: mpsc::Sender<WorkerMsg>,
    clips: mpsc::Sender<ClipData>,
) {
    match uni {
        Uni::Session { session, .. } => {
            tracing::debug!(%client, %session, "a client opened a session stream; ignored");
        }
        Uni::Conversation { session, .. } => {
            tracing::debug!(%client, %session, "a client opened a conversation stream; ignored");
        }
        Uni::Bulk { header, mut rx } => match header.purpose.clone() {
            Purpose::Upload => receive(daemon, header, rx, out).await,
            Purpose::Clip { generation, format } => {
                if let Some(bytes) = read_whole(&header, &mut rx, MAX_REP_BYTES).await {
                    let _sent = clips.send(ClipData { generation, format, bytes }).await;
                }
            }
            Purpose::Save { path, base_modified_ms } => {
                if let Some(text) = read_whole(&header, &mut rx, FILE_BYTES).await {
                    crate::files::write(client, &out, path, text, base_modified_ms).await;
                } else {
                    tracing::info!(%client, %path, size = header.size, "save stream refused");
                    let error = if header.size > FILE_BYTES {
                        format!("{} bytes is past the {FILE_BYTES}-byte cap", header.size)
                    } else {
                        "the save was cut off".to_owned()
                    };
                    let result = WriteResult::Failed { error };
                    let _sent = out.send(WorkerMsg::Written { path, result }).await;
                }
            }
            Purpose::Download | Purpose::FileText => {
                tracing::debug!(%client, "a download or a file's text sent up; refused");
                rx.stop();
            }
        },
    }
}

/// A stream's bytes, when it carries all its header announced and that is no more than `max`
/// (a clipboard representation, a file tile's save).
async fn read_whole(header: &BulkHeader, rx: &mut RawRecv, max: u64) -> Option<Vec<u8>> {
    if header.size > max {
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
        let clip = Arc::clone(&daemon.clip);
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
/// stream per file, each followed by its `Done` with the digest of the whole file. A file the
/// client holds part of (`held`) resumes where [`Transfers::resume_points`] says. Failures are
/// reported as `Failed`.
pub async fn download(
    transfers: Arc<Transfers>,
    conn: Connection,
    out: mpsc::Sender<WorkerMsg>,
    xfer: XferId,
    path: String,
    held: Vec<(String, u64)>,
) {
    let at = slopty_worker::file::expand_home(std::path::Path::new(&path));
    let listed = tokio::task::spawn_blocking(move || {
        let files = outgoing(&at)?;
        let from = transfers.resume_points(&files, &held);
        Ok::<_, std::io::Error>(files.into_iter().zip(from).collect::<Vec<_>>())
    })
    .await;
    let files = match listed {
        Ok(Ok(files)) => files,
        Ok(Err(e)) => return fail(&out, xfer, None, &e.to_string()).await,
        Err(e) => return fail(&out, xfer, None, &e.to_string()).await,
    };
    let bytes = files.iter().map(|(f, _from)| f.size).sum();
    let count = u32::try_from(files.len()).unwrap_or(u32::MAX);
    let resumed = files.iter().filter(|(_f, from)| *from > 0).count();
    tracing::info!(%xfer, %path, files = count, bytes, resumed, "download");
    let begin = XferMsg::Begin { xfer, dest: None, files: count, bytes };
    if out.send(WorkerMsg::Xfer(begin)).await.is_err() {
        return;
    }
    for (file, offset) in files {
        let header = BulkHeader {
            xfer,
            purpose: Purpose::Download,
            name: file.name.clone(),
            size: file.size,
            mtime_ms: file.mtime_ms,
            mode: file.mode,
            offset,
        };
        match send_file(&conn, header, &file.path).await {
            Ok(hash) => {
                let path = file.path.to_string_lossy().into_owned();
                let done = XferMsg::Done { xfer, name: file.name, path, hash };
                if out.send(WorkerMsg::Xfer(done)).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                tracing::info!(%xfer, name = %file.name, error = %e, "download failed");
                return fail(&out, xfer, Some(file.name), &e.to_string()).await;
            }
        }
    }
}

async fn fail(out: &mpsc::Sender<WorkerMsg>, xfer: XferId, name: Option<String>, error: &str) {
    let failed = XferMsg::Failed { xfer, name, error: error.to_owned() };
    let _sent = out.send(WorkerMsg::Xfer(failed)).await;
}

/// One file down a bulk stream: exactly the bytes its header announces, from its offset.
/// Returns the digest of the whole file, the bytes before the offset read from disk.
async fn send_file(
    conn: &Connection,
    header: BulkHeader,
    path: &std::path::Path,
) -> Result<Hash, NetError> {
    let io = |e| NetError::io(path.display(), e);
    let (size, offset) = (header.size, header.offset);
    let mut file = tokio::fs::File::open(path).await.map_err(io)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0_u8; CHUNK];
    let mut read = 0_u64;
    while read < offset {
        let want = usize::try_from(offset.saturating_sub(read)).unwrap_or(CHUNK).min(CHUNK);
        let n = file.read(buf.get_mut(..want).unwrap_or_default()).await.map_err(io)?;
        if n == 0 {
            let short = format!("shorter than the {offset} bytes to resume from");
            return Err(io(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, short)));
        }
        hasher.update(buf.get(..n).unwrap_or_default());
        read = read.saturating_add(n as u64);
    }
    let mut send = streams::open_bulk(conn, header).await?;
    let mut sent = offset;
    while sent < size {
        let n = file.read(&mut buf).await.map_err(io)?;
        if n == 0 {
            break;
        }
        let n = usize::try_from((n as u64).min(size.saturating_sub(sent))).unwrap_or(n);
        let bytes = buf.get(..n).unwrap_or_default();
        hasher.update(bytes);
        send.write_all(bytes).await.map_err(|e| NetError::stream(&e))?;
        sent = sent.saturating_add(n as u64);
    }
    send.finish().map_err(|e| NetError::stream(&e))?;
    Ok(hasher.finalize().into())
}

/// Answer a client's fetch of the worker's clipboard: inline when it fits, a bulk stream when
/// not, `Unavailable` when the pasteboard changed since the offer.
pub async fn send_clip(
    daemon: &Daemon,
    conn: &Connection,
    out: &mpsc::Sender<WorkerMsg>,
    generation: u64,
    format: ClipFormat,
) {
    use slopty_proto::transfer::ClipMsg;
    let Some(bytes) = daemon.clip.fetch(generation, format) else {
        let _sent = out.send(WorkerMsg::Clip(ClipMsg::Unavailable { generation })).await;
        return;
    };
    if bytes.len() <= INLINE_CLIP_BYTES {
        let data = ClipMsg::Data { generation, format, bytes: bytes.to_vec() };
        let _sent = out.send(WorkerMsg::Clip(data)).await;
        return;
    }
    let header = BulkHeader {
        xfer: XferId::new(),
        purpose: Purpose::Clip { generation, format },
        name: String::new(),
        size: bytes.len() as u64,
        mtime_ms: WallMs::ZERO,
        mode: 0,
        offset: 0,
    };
    let conn = conn.clone();
    drop(tokio::spawn(async move {
        let sent = async {
            let mut send = streams::open_bulk(&conn, header).await?;
            send.write_all(&bytes).await.map_err(|e| NetError::stream(&e))?;
            send.finish().map_err(|e| NetError::stream(&e))
        };
        if let Err(e) = sent.await {
            tracing::debug!(generation, error = %e, "clipboard data not sent");
        }
    }));
}

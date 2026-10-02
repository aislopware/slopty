//! The unidirectional streams a client opens (files and clipboard data coming up) and the
//! files it fetches (going down).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use slopty_core::{ClientId, WallMs, XferId};
use slopty_net::streams::{self, RawRecv, Uni};
use slopty_net::{Connection, NetError, WorkerMsg};
use slopty_proto::file::{FILE_BYTES, WriteResult};
use slopty_proto::transfer::{
    BulkHeader, Hash, Held, INLINE_CLIP_BYTES, Purpose, RepRef, Source, XferMsg,
};
use slopty_worker::clip::MAX_REP_BYTES;
use slopty_worker::screen::drag::Heard;
use slopty_worker::xfer::{Again, Claim, Landed, Receiving, XferError, outgoing, resume_points};
use tokio::io::AsyncReadExt as _;
use tokio::sync::mpsc;

use crate::Daemon;

/// Bytes read from a stream or a file at a time.
const CHUNK: usize = 256 << 10;
/// Chunks queued between a stream and the thread writing its file.
const QUEUED: usize = 16;
/// How long a file's stream waits for its transfer's `Begin`, which rides another stream.
pub const BEGIN_WAIT: Duration = Duration::from_secs(5);

/// Clipboard bytes a client sent up as a bulk stream, for the connection's loop.
#[derive(Debug)]
pub struct ClipData {
    /// Which representation of the client's offer.
    pub rep: RepRef,
    /// The bytes; `None` when the stream was refused (past [`MAX_REP_BYTES`], or longer than
    /// it said) or cut, so nothing of it is coming and a paste waiting on it goes on.
    pub bytes: Option<Vec<u8>>,
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
        Uni::Thread { thread, .. } => {
            tracing::debug!(%client, %thread, "a client opened a thread stream; ignored");
        }
        Uni::Bulk { header, mut rx } => match header.purpose.clone() {
            Purpose::Upload => receive(daemon, header, rx, out).await,
            Purpose::Rep { rep } => {
                let bytes = read_rep(&daemon, &header, &rep, &mut rx).await;
                if bytes.is_none() {
                    tracing::debug!(%client, item = rep.item, size = header.size, "clipboard stream refused or cut");
                }
                // A drag's data goes to the drag, never near the pasteboard.
                if let Source::Drag(drag) = rep.source {
                    let RepRef { item, kind, .. } = rep;
                    daemon.dnd.drags().tell(drag, Heard::Data { item, kind, bytes });
                    return;
                }
                let _sent = clips.send(ClipData { rep, bytes }).await;
            }
            Purpose::Save { path, base_modified_ms } => {
                if let Some(text) = read_whole(&header, &mut rx, FILE_BYTES).await {
                    let handoffs = &daemon.handoffs;
                    crate::files::write(handoffs, client, &out, path, text, base_modified_ms).await;
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
            Purpose::Download | Purpose::FileBody => {
                tracing::debug!(%client, "a download or a file's text sent up; refused");
                rx.stop();
            }
        },
    }
}

/// A clipboard representation's bytes, when its stream carries all its header announced and
/// that is no more than [`MAX_REP_BYTES`]. Each chunk tells the clipboard the bytes are still
/// coming, so a promise waiting on them waits on.
async fn read_rep(
    daemon: &Daemon,
    header: &BulkHeader,
    rep: &RepRef,
    rx: &mut RawRecv,
) -> Option<Vec<u8>> {
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
        daemon.clip.receiving(rep);
    }
    (bytes.len() as u64 == header.size).then_some(bytes)
}

/// A stream's bytes, when it carries all its header announced and that is no more than `max`
/// (a file tile's save).
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
/// resume). A file that landed before (the client's next link sends what it did not hear
/// landed) is reported again, with the transfer's end once it has ended.
async fn receive(
    daemon: Daemon,
    header: BulkHeader,
    mut rx: RawRecv,
    out: mpsc::Sender<WorkerMsg>,
) {
    let xfer = header.xfer;
    let name = header.name.clone();
    let landed = match write(&daemon, &header, &mut rx, &out).await {
        Ok(Some(Got::Landed(landed, claim))) => (landed, claim),
        Ok(Some(Got::Again(Again { landed, finished }))) => {
            tracing::debug!(%xfer, %name, "landed before; said again");
            let path = landed.path.to_string_lossy().into_owned();
            let done = XferMsg::Done { xfer, name, path, hash: landed.hash };
            let _sent = out.send(WorkerMsg::Xfer(done)).await;
            if let Some(finished) = finished {
                let paths = finished.paths.iter().map(|p| p.to_string_lossy().into_owned());
                let finished = XferMsg::Finished { xfer, paths: paths.collect() };
                let _sent = out.send(WorkerMsg::Xfer(finished)).await;
            }
            return;
        }
        Ok(None) => {
            tracing::debug!(%xfer, %name, "upload cancelled, or taken over by a later stream");
            return;
        }
        Err(e) => {
            tracing::info!(%xfer, %name, error = %e, "upload failed");
            if let Some(drag) = daemon.transfers.drag_of(xfer) {
                daemon.dnd.drags().tell(drag, Heard::Failed(format!("{name}: {e}")));
            }
            let failed = XferMsg::Failed { xfer, name: Some(name), error: e.to_string() };
            let _sent = out.send(WorkerMsg::Xfer(failed)).await;
            return;
        }
    };
    let (landed, claim) = landed;
    tracing::info!(%xfer, %name, path = %landed.path.display(), bytes = landed.size, "file landed");
    let path = landed.path.to_string_lossy().into_owned();
    let done = XferMsg::Done { xfer, name: name.clone(), path, hash: landed.hash };
    let _sent = out.send(WorkerMsg::Xfer(done)).await;
    // Held until the landing is recorded, so a later stream of the file finds it landed.
    let finished = daemon.transfers.landed(xfer, &name, landed);
    drop(claim);
    let Some(finished) = finished else { return };
    if let Some(drag) = finished.drag {
        for path in &finished.paths {
            let top = path.file_name().map(|n| n.to_string_lossy().into_owned());
            if let Some(top) = top {
                daemon.dnd.drags().tell(drag, Heard::Landed { name: top, path: path.clone() });
            }
        }
    }
    if finished.staging {
        let clip = Arc::clone(&daemon.clip);
        let paths = finished.paths.clone();
        let write = move || clip.write_files(&paths, Instant::now());
        let _written = tokio::task::spawn_blocking(write).await;
    }
    let paths = finished.paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let _sent = out.send(WorkerMsg::Xfer(XferMsg::Finished { xfer, paths })).await;
}

/// What one stream of an upload came to.
enum Got {
    /// Its file landed; the claim is held until the landing is recorded.
    Landed(Landed, Claim),
    /// Its file had landed before it came.
    Again(Again),
}

/// The stream of a file that landed before it came: whatever it carries is not written.
async fn landed_before(rx: &mut RawRecv, again: Again) -> Result<Option<Got>, XferError> {
    while rx.chunk(CHUNK).await.map_err(|e| XferError::Io(std::io::Error::other(e)))?.is_some() {}
    Ok(Some(Got::Again(again)))
}

/// Write the stream into its file on a blocking thread, reporting progress. `Ok(None)` when
/// the transfer was cancelled or a later stream of the same file took it over.
async fn write(
    daemon: &Daemon,
    header: &BulkHeader,
    rx: &mut RawRecv,
    out: &mpsc::Sender<WorkerMsg>,
) -> Result<Option<Got>, XferError> {
    let xfer = header.xfer;
    if !daemon.transfers.begun(xfer, BEGIN_WAIT).await {
        rx.stop();
        return Err(XferError::Unknown);
    }
    if let Some(again) = daemon.transfers.landed_before(xfer, &header.name) {
        return landed_before(rx, again).await;
    }
    let mut claim = daemon.transfers.claim(xfer, &header.name).await.inspect_err(|_e| rx.stop())?;
    // The stream this one took over from may have landed the file meanwhile.
    if let Some(again) = daemon.transfers.landed_before(xfer, &header.name) {
        return landed_before(rx, again).await;
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
    let mut stopped = false;
    let mut ended = false;
    loop {
        tokio::select! {
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow_and_update() {
                    stopped = true;
                    break;
                }
            }
            () = claim.superseded() => {
                tracing::debug!(%xfer, name = %header.name, "a later stream takes the file over");
                stopped = true;
                break;
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
        Err(_e) if stopped => Ok(None),
        other => other.map(|landed| Some(Got::Landed(landed, claim))),
    }
}

/// Send the file or directory at `path` down as transfer `xfer`: a `Begin`, then one bulk
/// stream per file, each followed by its `Done` with the digest of the whole file. A file the
/// client holds part of (`held`) resumes where [`resume_points`] says. Failures are
/// reported as `Failed`.
pub async fn download(
    conn: Connection,
    out: mpsc::Sender<WorkerMsg>,
    xfer: XferId,
    path: String,
    held: Vec<Held>,
) {
    let at = slopty_worker::file::expand_home(std::path::Path::new(&path));
    let listed = tokio::task::spawn_blocking(move || {
        let files = outgoing(&at)?;
        let from = resume_points(&files, &held);
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
/// not (ahead of background transfers when a paste waits on it), `TooBig` past the fetch's cap,
/// `Unavailable` when the pasteboard changed since the offer.
pub async fn send_clip(
    daemon: &Daemon,
    conn: &Connection,
    out: &mpsc::Sender<WorkerMsg>,
    rep: RepRef,
    max: Option<u64>,
    urgent: bool,
) {
    use slopty_proto::transfer::ClipMsg;
    use slopty_worker::clip::Fetched;
    let fetched = if let Source::Drag(drag) = rep.source {
        // What the catch of a drag out kept: the client's own drop is fetching it.
        match daemon.dnd.drags().kept(drag, rep.item, &rep.kind) {
            Some(bytes) => {
                let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                if max.is_some_and(|max| size > max) {
                    Fetched::TooBig(size)
                } else {
                    Fetched::Data(bytes)
                }
            }
            None => Fetched::Unavailable,
        }
    } else {
        let (clip, asked) = (Arc::clone(&daemon.clip), rep.clone());
        // Off the runtime: a representation the poll left alone is read off the pasteboard now.
        let fetched = tokio::task::spawn_blocking(move || clip.fetch(&asked, max)).await;
        fetched.unwrap_or(Fetched::Unavailable)
    };
    let bytes = match fetched {
        Fetched::Data(bytes) if bytes.len() > INLINE_CLIP_BYTES => bytes,
        Fetched::Data(bytes) => {
            let data = ClipMsg::Data { rep, bytes: bytes.to_vec() };
            let _sent = out.send(WorkerMsg::Clip(data)).await;
            return;
        }
        Fetched::TooBig(size) => {
            let _sent = out.send(WorkerMsg::Clip(ClipMsg::TooBig { rep, size })).await;
            return;
        }
        Fetched::Unavailable => {
            let source = rep.source;
            let _sent = out.send(WorkerMsg::Clip(ClipMsg::Unavailable { source })).await;
            return;
        }
    };
    let item = rep.item;
    let header = BulkHeader {
        xfer: XferId::new(),
        purpose: Purpose::Rep { rep },
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
            if urgent {
                // A paste waits on it: level with the tunnels, ahead of files.
                send.set_priority(streams::TUNNEL_PRIORITY).map_err(|e| NetError::stream(&e))?;
            }
            send.write_all(&bytes).await.map_err(|e| NetError::stream(&e))?;
            send.finish().map_err(|e| NetError::stream(&e))
        };
        if let Err(e) = sent.await {
            tracing::debug!(item, error = %e, "clipboard data not sent");
        }
    }));
}

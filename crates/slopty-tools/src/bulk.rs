//! Files of any size, moved between this machine and a worker in parts.
//!
//! **Up.** A file here, or standard input, is read part by part until it ends and sent as
//! [`UploadPart::Bytes`] steps, [`WINDOW`] of them in flight at once; the finish carries the
//! size and the BLAKE3 digest of what was read, and the worker puts the parts in place only
//! when they add up. Under an
//! idempotency key the upload is named for the key, so a call sent again writes into the same
//! parts and its finish answers what the first one did; the abort sent after it sweeps the
//! parts such a repeat wrote again.
//!
//! **Down.** [`Verb::ReadFile`] ranges, [`WINDOW`] at a time, written where they go in a
//! partial file beside the target and renamed over it at the end, or in order to standard
//! output. The file's size and time are looked at before and after, and a file that changed
//! meanwhile is refused rather than handed over half old and half new.
//!
//! A part is [`PART_BYTES`], well under a reply's cap: the parts share the server's links with
//! every other verb and event, and one part holds those up for a moment at most.

use std::fs::File;
use std::future::Future;
use std::io::{Read, Write};
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use slopty_core::{WorkerId, XferId};
use slopty_proto::orchestration::{
    ErrorCode, FileKind, FileStat, IdempotencyKey, Outcome, UploadPart, Verb,
};

use crate::resolve::Resolver;
use crate::{Dispatch, ToolError};

/// Bytes one part carries.
pub const PART_BYTES: u64 = 1 << 20;

/// Parts in flight at once.
pub const WINDOW: usize = 4;

/// A file moved between this machine and a worker.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Moved {
    /// The worker.
    pub worker: WorkerId,
    /// Its path there, as given.
    pub remote: String,
    /// The path here.
    pub local: PathBuf,
    /// Bytes.
    pub size: u64,
}

fn local_failure(path: &Path, e: &std::io::Error) -> ToolError {
    ToolError::new(ErrorCode::Failed, format!("{} (here): {e}", path.display()))
}

/// Run file work here on the blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| ToolError::new(ErrorCode::Failed, e.to_string()))?
}

/// Every future's output, the futures run together.
async fn together<F: Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut running: Vec<Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = running.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (future, output) in running.iter_mut().zip(outputs.iter_mut()) {
            if output.is_none() {
                match future.as_mut().poll(cx) {
                    Poll::Ready(done) => *output = Some(done),
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending { Poll::Pending } else { Poll::Ready(()) }
    })
    .await;
    outputs.into_iter().flatten().collect()
}

/// The parts of the bytes from `start` to `end`: each one's offset and length.
fn parts(start: u64, end: u64) -> impl Iterator<Item = (u64, u64)> {
    let step = usize::try_from(PART_BYTES).unwrap_or(usize::MAX);
    (start..end)
        .step_by(step)
        .map(move |offset| (offset, PART_BYTES.min(end.saturating_sub(offset))))
}

/// The upload a key names: the same key, the same parts on the worker.
fn upload_named(key: Option<&IdempotencyKey>) -> XferId {
    let named = key.and_then(|key| {
        let digest = blake3::hash(key.as_str().as_bytes()).to_hex();
        digest.as_str().get(..32)?.parse().ok()
    });
    named.unwrap_or_default()
}

/// The source a [`push`] reads, or the place a [`pull`] writes: a file here, or standard input
/// or output (`-`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Here {
    /// A file here; a download into a directory takes the file under its own name.
    Path(PathBuf),
    /// Standard input up, standard output down.
    Stdio,
}

impl Here {
    /// `-` for standard input or output, else the path.
    #[must_use]
    pub fn parse(arg: &str) -> Self {
        if arg == "-" { Self::Stdio } else { Self::Path(PathBuf::from(arg)) }
    }

    fn shown(&self) -> PathBuf {
        match self {
            Self::Path(path) => path.clone(),
            Self::Stdio => PathBuf::from("-"),
        }
    }
}

/// Send `from`, a file here or standard input until it ends, to `remote` on a worker.
///
/// It replaces what is there. A file replaced there keeps its mode; a new one takes the
/// file's here, or the worker's default for standard input.
///
/// # Errors
///
/// A file here that cannot be read or is not a regular file, and whatever the worker refuses;
/// the parts sent are dropped then.
pub async fn push<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    from: &Here,
    remote: String,
    key: Option<IdempotencyKey>,
) -> Result<Moved, ToolError> {
    match from {
        Here::Path(path) => {
            let opened = path.clone();
            let (file, mode) = blocking(move || {
                let file = File::open(&opened).map_err(|e| local_failure(&opened, &e))?;
                let meta = file.metadata().map_err(|e| local_failure(&opened, &e))?;
                if !meta.is_file() {
                    let message = format!("{} (here) is not a regular file", opened.display());
                    return Err(ToolError::new(ErrorCode::Failed, message));
                }
                let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o7777;
                Ok((file, mode))
            })
            .await?;
            upload(res, worker, (file, path.clone(), Some(mode)), remote, key).await
        }
        Here::Stdio => {
            let stdin = (std::io::stdin(), PathBuf::from("-"), None);
            upload(res, worker, stdin, remote, key).await
        }
    }
}

/// Send what `reader` reads, until it ends, to `remote` on a worker in parts: [`WINDOW`]
/// parts read here, then sent together, until a short one. `shown` names it in a failure and
/// in what is moved.
async fn upload<D: Dispatch, R: Read + Send + 'static>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    (reader, shown, mode): (R, PathBuf, Option<u32>),
    remote: String,
    key: Option<IdempotencyKey>,
) -> Result<Moved, ToolError> {
    let worker = res.worker(worker).await?;
    let dispatch = res.dispatch();
    let upload = upload_named(key.as_ref());
    let step = |part| Verb::Upload { worker, path: remote.clone(), upload, part };
    let mut hasher = blake3::Hasher::new();
    let mut size = 0_u64;
    let mut reader = Some(reader);
    loop {
        let Some(taken) = reader.take() else { break };
        let named = shown.clone();
        let read = blocking(move || {
            let mut taken = taken;
            let batch = read_batch(&mut taken, &named)?;
            Ok((taken, batch))
        })
        .await;
        let sent = match read {
            Ok((back, batch)) => {
                let ended = batch.last().is_none_or(|part| (part.len() as u64) < PART_BYTES);
                if !ended {
                    reader = Some(back);
                }
                let mut steps = Vec::with_capacity(batch.len());
                for bytes in batch {
                    hasher.update(&bytes);
                    let offset = size;
                    size = size.saturating_add(bytes.len() as u64);
                    steps.push(dispatch.call(step(UploadPart::Bytes { offset, bytes })));
                }
                together(steps).await.into_iter().try_for_each(|outcome| match outcome {
                    Outcome::Done => Ok(()),
                    other => Err(ToolError::unexpected(other)),
                })
            }
            Err(e) => Err(e),
        };
        if let Err(e) = sent {
            let _dropped = dispatch.call(step(UploadPart::Abort)).await;
            return Err(e);
        }
    }
    let digest = *hasher.finalize().as_bytes();
    let keyed = key.is_some();
    let finish = UploadPart::Finish { size, digest, mode };
    match dispatch.send(key, step(finish)).await {
        Outcome::Done => {}
        other => return Err(ToolError::unexpected(other)),
    }
    if keyed {
        // A repeat of a finished upload wrote its parts again; its finish answered from the
        // worker's table and left them.
        let _swept = dispatch.call(step(UploadPart::Abort)).await;
    }
    Ok(Moved { worker, remote, local: shown, size })
}

/// Up to [`WINDOW`] parts from `reader`, each whole but the last one read before it ended,
/// which may be short or empty.
fn read_batch(reader: &mut impl Read, shown: &Path) -> Result<Vec<Vec<u8>>, ToolError> {
    let part = usize::try_from(PART_BYTES).unwrap_or(usize::MAX);
    let mut batch = Vec::with_capacity(WINDOW);
    while batch.len() < WINDOW {
        let mut bytes = Vec::with_capacity(part);
        let read = reader
            .by_ref()
            .take(PART_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|e| local_failure(shown, &e))?;
        let whole = read == part;
        if read > 0 {
            batch.push(bytes);
        }
        if !whole {
            break;
        }
    }
    Ok(batch)
}

/// Bring `remote` from a worker to `to`, a file here or standard output.
///
/// A file here is replaced, and a directory takes it under its own name. `range` (an offset
/// and at most a length) takes only part of it, to standard output only.
///
/// The bytes are of one version of the file: one that changes while it is read is refused.
/// Into a file here nothing of it is left then; standard output has had what came before.
///
/// # Errors
///
/// Nothing at `remote` or not a regular file there, a range into a file, a file that changed
/// while it was read, and whatever the disk here refuses.
pub async fn pull<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    remote: String,
    to: &Here,
    range: (u64, Option<u64>),
) -> Result<Moved, ToolError> {
    match to {
        Here::Path(local) if range == (0, None) => download(res, worker, remote, local).await,
        Here::Path(_) => Err(ToolError::invalid("a range goes to standard output (`-`) only")),
        Here::Stdio => stream(res, worker, remote, range, std::io::stdout())
            .await
            .map(|moved| Moved { local: to.shown(), ..moved }),
    }
}

/// `remote`'s kind and size, refused when it is no regular file.
async fn regular<D: Dispatch>(
    dispatch: &D,
    worker: WorkerId,
    remote: &str,
) -> Result<FileStat, ToolError> {
    match dispatch.call(Verb::Stat { worker, path: remote.to_owned() }).await {
        Outcome::Stat(Some(stat)) if stat.kind == FileKind::File => Ok(stat),
        Outcome::Stat(Some(_other)) => {
            let message = format!("{remote} is not a regular file on the worker");
            Err(ToolError::new(ErrorCode::Failed, message))
        }
        Outcome::Stat(None) => {
            Err(ToolError::new(ErrorCode::Failed, format!("nothing is at {remote}")))
        }
        other => Err(ToolError::unexpected(other)),
    }
}
/// Bring `remote` to the file `local` here through a partial file beside it, renamed over it
/// once whole.
async fn download<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    remote: String,
    local: &Path,
) -> Result<Moved, ToolError> {
    let worker = res.worker(worker).await?;
    let dispatch = res.dispatch();
    let before = regular(dispatch, worker, &remote).await?;
    let size = before.size;
    let name = Path::new(&remote).file_name().map(ToOwned::to_owned);
    let (target, partial, file) = {
        let local = local.to_path_buf();
        blocking(move || open_partial(&local, name.as_deref(), size)).await?
    };
    let put = async |chunks: Vec<(u64, Vec<u8>)>| {
        let (file, partial) = (Arc::clone(&file), partial.clone());
        blocking(move || {
            chunks.iter().try_for_each(|(offset, bytes)| {
                file.write_all_at(bytes, *offset).map_err(|e| local_failure(&partial, &e))
            })
        })
        .await
    };
    let fetched = fetch(dispatch, worker, &remote, &before, (0, size), put).await;
    let (placed_at, removed_at) = (target.clone(), partial.clone());
    blocking(move || {
        let placed = fetched.and_then(|()| {
            file.sync_all().map_err(|e| local_failure(&partial, &e))?;
            std::fs::rename(&partial, &placed_at).map_err(|e| local_failure(&placed_at, &e))
        });
        if placed.is_err() {
            let _removed = std::fs::remove_file(&removed_at);
        }
        placed
    })
    .await?;
    Ok(Moved { worker, remote, local: target, size })
}

/// Write `remote`, or the part of it `(offset, length)` names, to `out` in order.
async fn stream<D: Dispatch, W: Write + Send + 'static>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    remote: String,
    (offset, length): (u64, Option<u64>),
    out: W,
) -> Result<Moved, ToolError> {
    let worker = res.worker(worker).await?;
    let dispatch = res.dispatch();
    let before = regular(dispatch, worker, &remote).await?;
    let end = length.map_or(before.size, |n| offset.saturating_add(n).min(before.size));
    let start = offset.min(end);
    let mut out = Some(out);
    let put = async |chunks: Vec<(u64, Vec<u8>)>| {
        let Some(mut writer) = out.take() else {
            return Err(ToolError::new(ErrorCode::Failed, "standard output was lost"));
        };
        let writer = blocking(move || {
            let written = chunks
                .iter()
                .try_for_each(|(_offset, bytes)| writer.write_all(bytes))
                .and_then(|()| writer.flush());
            written.map_err(|e| local_failure(Path::new("-"), &e))?;
            Ok(writer)
        })
        .await?;
        out = Some(writer);
        Ok(())
    };
    fetch(dispatch, worker, &remote, &before, (start, end), put).await?;
    let size = end.saturating_sub(start);
    Ok(Moved { worker, remote, local: PathBuf::from("-"), size })
}

/// Read `remote` from `start` to `end` in parts, [`WINDOW`] at a time, each batch handed to
/// `put` in order; then refuse it if it is no longer the file `before` saw.
async fn fetch<D: Dispatch>(
    dispatch: &D,
    worker: WorkerId,
    remote: &str,
    before: &FileStat,
    (start, end): (u64, u64),
    mut put: impl AsyncFnMut(Vec<(u64, Vec<u8>)>) -> Result<(), ToolError>,
) -> Result<(), ToolError> {
    let changed = || {
        ToolError::new(ErrorCode::Failed, format!("{remote} changed while it was read; try again"))
    };
    let all: Vec<(u64, u64)> = parts(start, end).collect();
    for batch in all.chunks(WINDOW) {
        let reads = batch.iter().map(|&(offset, length)| {
            let path = remote.to_owned();
            dispatch.call(Verb::ReadFile { worker, path, offset, length: Some(length) })
        });
        let mut chunks = Vec::with_capacity(batch.len());
        for (outcome, &(offset, length)) in together(reads.collect()).await.into_iter().zip(batch) {
            match outcome {
                Outcome::File { bytes, size, .. } => {
                    let whole = u64::try_from(bytes.len()).is_ok_and(|n| n == length);
                    if size != before.size || !whole {
                        return Err(changed());
                    }
                    chunks.push((offset, bytes));
                }
                other => return Err(ToolError::unexpected(other)),
            }
        }
        put(chunks).await?;
    }
    match regular(dispatch, worker, remote).await {
        Ok(after) if after.size == before.size && after.modified_ms == before.modified_ms => Ok(()),
        Ok(_) | Err(_) => Err(changed()),
    }
}

/// Where a download to `local` lands (inside it, under `name`, when it is a directory), and a
/// partial file of `size` bytes beside that.
fn open_partial(
    local: &Path,
    name: Option<&std::ffi::OsStr>,
    size: u64,
) -> Result<(PathBuf, PathBuf, Arc<File>), ToolError> {
    let target = match name {
        Some(name) if local.is_dir() => local.join(name),
        _ => local.to_path_buf(),
    };
    let file_name = target
        .file_name()
        .ok_or_else(|| ToolError::invalid(format!("{} (here) names no file", target.display())))?;
    let dir =
        target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let partial = dir.join(format!(".{}.slopty-download", file_name.to_string_lossy()));
    let file = File::create(&partial).map_err(|e| local_failure(&partial, &e))?;
    file.set_len(size).map_err(|e| local_failure(&partial, &e))?;
    Ok((target, partial, Arc::new(file)))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use parking_lot::Mutex;
    use slopty_core::WallMs;
    use slopty_proto::orchestration::FileStat;
    use slopty_proto::server::{Liveness, Os, WorkerCaps, WorkerInfo};

    use super::*;

    fn studio() -> WorkerId {
        "0199a000-0000-7000-8000-000000000001".parse().unwrap()
    }

    /// A worker whose files live in memory: uploads gather their parts, reads answer ranges,
    /// and a file can be made to change between two looks at it. Counts the reads and parts in
    /// flight at once.
    #[derive(Default)]
    struct Worker {
        files: Mutex<HashMap<String, Vec<u8>>>,
        parts: Mutex<HashMap<XferId, Vec<u8>>>,
        steps: Mutex<Vec<String>>,
        /// Touches the file after this many reads.
        touch_after: Option<usize>,
        reads: AtomicUsize,
        in_flight: AtomicUsize,
        most_in_flight: AtomicUsize,
    }

    impl Worker {
        fn file(&self, path: &str) -> Option<Vec<u8>> {
            self.files.lock().get(path).cloned()
        }

        fn write_part(&self, upload: XferId, offset: u64, bytes: &[u8]) {
            let at = usize::try_from(offset).unwrap();
            let end = at.saturating_add(bytes.len());
            let mut parts = self.parts.lock();
            let held = parts.entry(upload).or_default();
            if held.len() < end {
                held.resize(end, 0);
            }
            held[at..end].copy_from_slice(bytes);
            drop(parts);
        }

        /// Change the file's first byte, and so its time.
        fn touch(&self, path: &str) {
            if let Some(bytes) = self.files.lock().get_mut(path) {
                bytes[0] = bytes[0].wrapping_add(1);
            }
        }

        async fn flight<T>(&self, answer: T) -> T {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst).saturating_add(1);
            self.most_in_flight.fetch_max(now, Ordering::SeqCst);
            tokio::task::yield_now().await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            answer
        }
    }

    impl Dispatch for Worker {
        async fn send(&self, _key: Option<IdempotencyKey>, verb: Verb) -> Outcome {
            match verb {
                Verb::ListWorkers => Outcome::Workers(vec![WorkerInfo {
                    worker: studio(),
                    name: "mac-studio".to_owned(),
                    address: String::new(),
                    liveness: Liveness::Online,
                    caps: WorkerCaps::bare(Os::MacOs),
                    load: 0.0,
                    last_seen_ms: WallMs::ZERO,
                }]),
                Verb::Upload { path, upload, part, .. } => {
                    let step = match &part {
                        UploadPart::Bytes { offset, .. } => format!("part {offset}"),
                        UploadPart::Finish { .. } => "finish".to_owned(),
                        UploadPart::Abort => "abort".to_owned(),
                    };
                    self.steps.lock().push(step);
                    match part {
                        UploadPart::Bytes { offset, bytes } => {
                            self.write_part(upload, offset, &bytes);
                            self.flight(Outcome::Done).await
                        }
                        UploadPart::Finish { size, digest, .. } => {
                            let held = self.parts.lock().remove(&upload).unwrap_or_default();
                            let adds_up = u64::try_from(held.len()).unwrap() == size
                                && blake3::hash(&held).as_bytes() == &digest;
                            if !adds_up {
                                return Outcome::Error {
                                    code: ErrorCode::Failed,
                                    message: "does not add up".to_owned(),
                                };
                            }
                            self.files.lock().insert(path, held);
                            Outcome::Done
                        }
                        UploadPart::Abort => {
                            self.parts.lock().remove(&upload);
                            Outcome::Done
                        }
                    }
                }
                Verb::Stat { path, .. } => {
                    let found = self.file(&path).map(|bytes| FileStat {
                        kind: FileKind::File,
                        size: u64::try_from(bytes.len()).unwrap(),
                        modified_ms: WallMs::from_millis(u64::from(
                            bytes.first().copied().unwrap_or(0),
                        )),
                        mode: 0o644,
                    });
                    Outcome::Stat(found)
                }
                Verb::ReadFile { path, offset, length, .. } => {
                    let reads = self.reads.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                    if self.touch_after == Some(reads) {
                        self.touch(&path);
                    }
                    let bytes = self.file(&path).unwrap_or_default();
                    let size = u64::try_from(bytes.len()).unwrap();
                    let at = usize::try_from(offset).unwrap().min(bytes.len());
                    let end = at.saturating_add(usize::try_from(length.unwrap()).unwrap());
                    let part = bytes[at..end.min(bytes.len())].to_vec();
                    self.flight(Outcome::File { bytes: part, offset, size }).await
                }
                other => panic!("not for this worker: {other:?}"),
            }
        }
    }

    /// Three and a half parts' worth of bytes that differ from part to part.
    fn contents() -> Vec<u8> {
        let size = usize::try_from(PART_BYTES.saturating_mul(7).checked_div(2).unwrap()).unwrap();
        (0..size).map(|i| u8::try_from(i.checked_rem(251).unwrap()).unwrap()).collect()
    }

    /// Up in parts, several in flight, the finish last; down in ranges into a file that holds
    /// the same bytes; a directory here takes the file under its own name.
    #[tokio::test]
    async fn a_file_goes_up_in_parts_and_comes_down_in_ranges() {
        let worker = Worker::default();
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("app.tar");
        std::fs::write(&here, contents()).unwrap();
        let mut res = Resolver::new(&worker);
        let from = Here::Path(here.clone());
        let moved = push(&mut res, None, &from, "~/app.tar".to_owned(), None).await.unwrap();
        assert_eq!(
            (moved.worker, moved.size),
            (studio(), u64::try_from(contents().len()).unwrap())
        );
        assert_eq!(worker.file("~/app.tar"), Some(contents()));
        let steps = worker.steps.lock().clone();
        assert_eq!(steps.len(), 5, "four parts and the finish: {steps:?}");
        assert_eq!(steps.last().map(String::as_str), Some("finish"));
        assert!(worker.most_in_flight.load(Ordering::SeqCst) > 1, "parts go several at a time");

        let back = dir.path().join("back");
        std::fs::create_dir_all(&back).unwrap();
        let to = Here::Path(back.clone());
        let moved = pull(&mut res, None, "~/app.tar".to_owned(), &to, (0, None)).await.unwrap();
        assert_eq!(moved.local, back.join("app.tar"));
        assert_eq!(std::fs::read(back.join("app.tar")).unwrap(), contents());
        let left: Vec<_> = std::fs::read_dir(&back).unwrap().collect();
        assert_eq!(left.len(), 1, "no partial file left here");
    }

    /// An upload under a key is named for it, and the abort after the finish sweeps what a
    /// repeat wrote again.
    #[tokio::test]
    async fn a_keyed_upload_sweeps_its_parts_after_the_finish() {
        let worker = Worker::default();
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("small");
        std::fs::write(&here, b"hello").unwrap();
        let key = IdempotencyKey::new("push-1").unwrap();
        let mut res = Resolver::new(&worker);
        let from = Here::Path(here);
        push(&mut res, None, &from, "/w/small".to_owned(), Some(key.clone())).await.unwrap();
        assert_eq!(*worker.steps.lock(), ["part 0", "finish", "abort"]);
        assert_eq!(
            upload_named(Some(&key)),
            upload_named(Some(&key)),
            "the same key, the same name"
        );
        assert_ne!(upload_named(None), upload_named(None));
        let nope = dir.path().join("nope");
        let missing = push(&mut res, None, &Here::Path(nope), "/w/x".to_owned(), None).await;
        assert_eq!(missing.unwrap_err().code, ErrorCode::Failed);
    }

    /// A file that changes while it is read is refused, and nothing is left here.
    #[tokio::test]
    async fn a_file_that_changes_while_it_is_read_is_refused() {
        let worker = Worker { touch_after: Some(2), ..Worker::default() };
        worker.files.lock().insert("/w/log".to_owned(), contents());
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("log");
        let mut res = Resolver::new(&worker);
        let to = Here::Path(here);
        let refused = pull(&mut res, None, "/w/log".to_owned(), &to, (0, None)).await.unwrap_err();
        assert!(refused.message.contains("changed while it was read"), "{refused}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "no partial left here");
        let missing = pull(&mut res, None, "/w/none".to_owned(), &to, (0, None)).await.unwrap_err();
        assert!(missing.message.contains("nothing is at"), "{missing}");
    }

    /// What standard output is handed, kept to look at.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A stream with no size up front goes up in parts until it ends: a short last part, a
    /// whole number of parts, and nothing at all each add up at the finish.
    #[tokio::test]
    async fn a_stream_goes_up_until_it_ends() {
        let worker = Worker::default();
        let mut res = Resolver::new(&worker);
        let whole = usize::try_from(PART_BYTES.saturating_mul(4)).unwrap();
        let even: Vec<u8> = contents().into_iter().cycle().take(whole).collect();
        for (bytes, steps) in [(contents(), 5), (even, 5), (Vec::new(), 1)] {
            worker.steps.lock().clear();
            let read = (std::io::Cursor::new(bytes.clone()), PathBuf::from("-"), None);
            let moved = upload(&mut res, None, read, "/w/in".to_owned(), None).await.unwrap();
            assert_eq!(moved.size, u64::try_from(bytes.len()).unwrap());
            assert_eq!(moved.local, Path::new("-"));
            assert_eq!(worker.file("/w/in"), Some(bytes));
            let sent = worker.steps.lock().clone();
            assert_eq!(sent.len(), steps, "{sent:?}");
            assert_eq!(sent.last().map(String::as_str), Some("finish"));
        }
    }

    /// Standard output takes the file, or a range of it, in order across parts; a range into a
    /// file here is refused, and a file that changes while it streams fails.
    #[tokio::test]
    async fn a_range_streams_out_in_order() {
        let worker = Worker::default();
        worker.files.lock().insert("/w/log".to_owned(), contents());
        let mut res = Resolver::new(&worker);
        let out = Shared::default();
        let all = stream(&mut res, None, "/w/log".to_owned(), (0, None), out.clone()).await;
        assert_eq!(all.unwrap().size, u64::try_from(contents().len()).unwrap());
        assert_eq!(*out.0.lock(), contents());

        let (offset, length) = (PART_BYTES / 2, PART_BYTES.saturating_mul(2));
        let out = Shared::default();
        let range = (offset, Some(length));
        let part = stream(&mut res, None, "/w/log".to_owned(), range, out.clone()).await.unwrap();
        assert_eq!(part.size, length);
        let (from, to) =
            (usize::try_from(offset).unwrap(), usize::try_from(offset + length).unwrap());
        assert_eq!(*out.0.lock(), contents()[from..to]);
        let past = stream(&mut res, None, "/w/log".to_owned(), (u64::MAX, None), Shared::default());
        assert_eq!(past.await.unwrap().size, 0, "past the end, nothing");

        let into = Here::Path(PathBuf::from("/tmp/x"));
        let refused = pull(&mut res, None, "/w/log".to_owned(), &into, range).await.unwrap_err();
        assert_eq!(refused.code, ErrorCode::Invalid, "{refused}");

        let touched = Worker { touch_after: Some(2), ..Worker::default() };
        touched.files.lock().insert("/w/log".to_owned(), contents());
        let mut res = Resolver::new(&touched);
        let changed = stream(&mut res, None, "/w/log".to_owned(), (0, None), Shared::default());
        let changed = changed.await.unwrap_err();
        assert!(changed.message.contains("changed while it was read"), "{changed}");
    }
}

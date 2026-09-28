//! Files of any size, moved between this machine and a worker in parts.
//!
//! **Up.** The file is read here part by part and sent as [`UploadPart::Bytes`] steps,
//! [`WINDOW`] of them in flight at once; the finish carries the size and the BLAKE3 digest of
//! what was read, and the worker puts the parts in place only when they add up. Under an
//! idempotency key the upload is named for the key, so a call sent again writes into the same
//! parts and its finish answers what the first one did; the abort sent after it sweeps the
//! parts such a repeat wrote again.
//!
//! **Down.** [`Verb::ReadFile`] ranges, [`WINDOW`] at a time, written where they go in a
//! partial file beside the target and renamed over it at the end. The file's size and time are
//! looked at before and after, and a file that changed meanwhile is refused rather than handed
//! over half old and half new.
//!
//! A part is [`PART_BYTES`], well under a reply's cap: the parts share the server's links with
//! every other verb and event, and one part holds those up for a moment at most.

use std::fs::File;
use std::future::Future;
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use slopty_core::{WorkerId, XferId};
use slopty_proto::orchestration::{ErrorCode, FileKind, IdempotencyKey, Outcome, UploadPart, Verb};

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

/// The parts of a file of `size` bytes: each one's offset and length.
fn parts(size: u64) -> impl Iterator<Item = (u64, u64)> {
    let step = usize::try_from(PART_BYTES).unwrap_or(usize::MAX);
    (0..size).step_by(step).map(move |offset| (offset, PART_BYTES.min(size.saturating_sub(offset))))
}

/// The upload a key names: the same key, the same parts on the worker.
fn upload_named(key: Option<&IdempotencyKey>) -> XferId {
    let named = key.and_then(|key| {
        let digest = blake3::hash(key.as_str().as_bytes()).to_hex();
        digest.as_str().get(..32)?.parse().ok()
    });
    named.unwrap_or_default()
}

/// Send the file `local` here to `remote` on a worker, replacing what is there.
///
/// # Errors
///
/// A file here that cannot be read or is not a regular file, and whatever the worker refuses;
/// the parts sent are dropped then.
pub async fn upload<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    local: &Path,
    remote: String,
    key: Option<IdempotencyKey>,
) -> Result<Moved, ToolError> {
    let worker = res.worker(worker).await?;
    let dispatch = res.dispatch();
    let path = local.to_path_buf();
    let (file, size, mode) = blocking(move || {
        let file = File::open(&path).map_err(|e| local_failure(&path, &e))?;
        let meta = file.metadata().map_err(|e| local_failure(&path, &e))?;
        if !meta.is_file() {
            let message = format!("{} (here) is not a regular file", path.display());
            return Err(ToolError::new(ErrorCode::Failed, message));
        }
        let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o7777;
        Ok((Arc::new(file), meta.len(), mode))
    })
    .await?;
    let upload = upload_named(key.as_ref());
    let step = |part| Verb::Upload { worker, path: remote.clone(), upload, part };
    let mut hasher = blake3::Hasher::new();
    let all: Vec<(u64, u64)> = parts(size).collect();
    for batch in all.chunks(WINDOW) {
        let (file, local, batch) = (Arc::clone(&file), local.to_path_buf(), batch.to_vec());
        let read = blocking(move || read_parts(&file, &local, &batch)).await;
        let sent = match read {
            Ok(read) => {
                for (_offset, bytes) in &read {
                    hasher.update(bytes);
                }
                let steps = read.into_iter().map(|(offset, bytes)| {
                    dispatch.call(step(UploadPart::Bytes { offset, bytes }))
                });
                together(steps.collect()).await.into_iter().try_for_each(|outcome| match outcome {
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
    let finish = UploadPart::Finish { size, digest, mode: Some(mode) };
    match dispatch.send(key, step(finish)).await {
        Outcome::Done => {}
        other => return Err(ToolError::unexpected(other)),
    }
    if keyed {
        // A repeat of a finished upload wrote its parts again; its finish answered from the
        // worker's table and left them.
        let _swept = dispatch.call(step(UploadPart::Abort)).await;
    }
    Ok(Moved { worker, remote, local: local.to_path_buf(), size })
}

/// The bytes of each `(offset, length)` of `file`.
fn read_parts(
    file: &File,
    path: &Path,
    parts: &[(u64, u64)],
) -> Result<Vec<(u64, Vec<u8>)>, ToolError> {
    parts
        .iter()
        .map(|&(offset, length)| {
            let mut bytes = vec![0; usize::try_from(length).unwrap_or(0)];
            file.read_exact_at(&mut bytes, offset).map_err(|e| local_failure(path, &e))?;
            Ok((offset, bytes))
        })
        .collect()
}

/// Bring `remote` from a worker to `local` here, replacing what is there; `local` may be a
/// directory, which takes the file under its own name.
///
/// # Errors
///
/// Nothing at `remote` or not a regular file there, a file that changed while it was read,
/// and whatever the disk here refuses.
pub async fn download<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    remote: String,
    local: &Path,
) -> Result<Moved, ToolError> {
    let worker = res.worker(worker).await?;
    let dispatch = res.dispatch();
    let stat = || dispatch.call(Verb::Stat { worker, path: remote.clone() });
    let before = match stat().await {
        Outcome::Stat(Some(stat)) if stat.kind == FileKind::File => stat,
        Outcome::Stat(Some(_other)) => {
            let message = format!("{remote} is not a regular file on the worker");
            return Err(ToolError::new(ErrorCode::Failed, message));
        }
        Outcome::Stat(None) => {
            return Err(ToolError::new(ErrorCode::Failed, format!("nothing is at {remote}")));
        }
        other => return Err(ToolError::unexpected(other)),
    };
    let size = before.size;
    let name = Path::new(&remote).file_name().map(ToOwned::to_owned);
    let (target, partial, file) = {
        let local = local.to_path_buf();
        blocking(move || open_partial(&local, name.as_deref(), size)).await?
    };
    let changed = || {
        ToolError::new(ErrorCode::Failed, format!("{remote} changed while it was read; try again"))
    };
    let all: Vec<(u64, u64)> = parts(size).collect();
    let fetched = async {
        for batch in all.chunks(WINDOW) {
            let reads = batch.iter().map(|&(offset, length)| {
                let verb =
                    Verb::ReadFile { worker, path: remote.clone(), offset, length: Some(length) };
                dispatch.call(verb)
            });
            let mut chunks = Vec::with_capacity(batch.len());
            for (outcome, &(offset, length)) in
                together(reads.collect()).await.into_iter().zip(batch)
            {
                match outcome {
                    Outcome::File { bytes, size: now, .. } => {
                        let whole = u64::try_from(bytes.len()).is_ok_and(|n| n == length);
                        if now != size || !whole {
                            return Err(changed());
                        }
                        chunks.push((offset, bytes));
                    }
                    other => return Err(ToolError::unexpected(other)),
                }
            }
            let (file, partial) = (Arc::clone(&file), partial.clone());
            blocking(move || {
                chunks.iter().try_for_each(|(offset, bytes)| {
                    file.write_all_at(bytes, *offset).map_err(|e| local_failure(&partial, &e))
                })
            })
            .await?;
        }
        match stat().await {
            Outcome::Stat(Some(after))
                if after.size == before.size && after.modified_ms == before.modified_ms =>
            {
                Ok(())
            }
            Outcome::Stat(_) => Err(changed()),
            other => Err(ToolError::unexpected(other)),
        }
    };
    let fetched = fetched.await;
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
        let moved = upload(&mut res, None, &here, "~/app.tar".to_owned(), None).await.unwrap();
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
        let moved = download(&mut res, None, "~/app.tar".to_owned(), &back).await.unwrap();
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
        upload(&mut res, None, &here, "/w/small".to_owned(), Some(key.clone())).await.unwrap();
        assert_eq!(*worker.steps.lock(), ["part 0", "finish", "abort"]);
        assert_eq!(
            upload_named(Some(&key)),
            upload_named(Some(&key)),
            "the same key, the same name"
        );
        assert_ne!(upload_named(None), upload_named(None));
        let nope = dir.path().join("nope");
        let missing = upload(&mut res, None, &nope, "/w/x".to_owned(), None).await;
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
        let refused = download(&mut res, None, "/w/log".to_owned(), &here).await.unwrap_err();
        assert!(refused.message.contains("changed while it was read"), "{refused}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "no partial left here");
        let missing = download(&mut res, None, "/w/none".to_owned(), &here).await.unwrap_err();
        assert!(missing.message.contains("nothing is at"), "{missing}");
    }
}

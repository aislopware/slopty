//! A worker's threads kept on this device: each followed thread's last state with its cursor, so
//! it draws in the first frame and catches up from there, and the outbox, so an intent survives a
//! relaunch.
//!
//! One directory per worker, the user's alone (0700), each file the user's alone (0600): a
//! thread holds what the person and the agent wrote. A thread is `<id>.thread`, the outbox is
//! `outbox`, each in postcard and replaced whole (`slopty_platform::fs::replace`), so a crash
//! mid-write leaves the one before. Only the [`CACHE_THREADS`] most recently written threads
//! are kept. Reads and writes are plain file IO: the caller runs them off its UI thread, except
//! the one read that draws a thread's first frame.

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::{fs, io};

use serde::{Deserialize, Serialize};
use slopty_proto::codec;
use slopty_proto::thread::{Cursor, ThreadId, ThreadState};

use super::Outbox;

/// Threads kept per worker; the least recently written go past it.
pub const CACHE_THREADS: usize = 64;

const THREAD_EXT: &str = "thread";
const OUTBOX: &str = "outbox";

/// A thread as kept: its state and the cursor it stands at.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Cached {
    /// Where the state stands in the worker's log.
    pub cursor: Cursor,
    /// The state.
    pub state: ThreadState,
}

/// Why the cache could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// The file system refused.
    #[error("thread cache: {0}")]
    Io(#[from] io::Error),
    /// A file did not decode: from another build, or cut short.
    #[error("thread cache: {0}")]
    Codec(#[from] codec::CodecError),
}

/// One worker's thread cache.
#[derive(Clone, Debug)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// The cache under `dir`, made when first written.
    #[must_use]
    pub const fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Where it is.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn thread_file(&self, thread: ThreadId) -> PathBuf {
        self.dir.join(format!("{thread}.{THREAD_EXT}"))
    }

    /// The thread as last kept; `None` when it was not, or the file is unreadable (then it goes).
    #[must_use]
    pub fn thread(&self, thread: ThreadId) -> Option<Cached> {
        let path = self.thread_file(thread);
        match read(&path) {
            Ok(found) => found,
            Err(error) => {
                tracing::debug!(%error, path = %path.display(), "thread cache dropped");
                let _gone = fs::remove_file(&path);
                None
            }
        }
    }

    /// Keep `cached` as `thread`, then let the least recently written go past
    /// [`CACHE_THREADS`].
    ///
    /// # Errors
    /// The file system's, or the encoder's.
    pub fn keep_thread(&self, thread: ThreadId, cached: &Cached) -> Result<(), CacheError> {
        self.write(&self.thread_file(thread), cached)?;
        self.prune()?;
        Ok(())
    }

    /// Forget `thread`.
    ///
    /// # Errors
    /// The file system's, other than the file being gone already.
    pub fn forget_thread(&self, thread: ThreadId) -> Result<(), CacheError> {
        match fs::remove_file(self.thread_file(thread)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// The outbox as last kept; empty when none was, or it is unreadable.
    #[must_use]
    pub fn outbox(&self) -> Outbox {
        read(&self.dir.join(OUTBOX)).ok().flatten().unwrap_or_default()
    }

    /// Keep the outbox.
    ///
    /// # Errors
    /// The file system's, or the encoder's.
    pub fn keep_outbox(&self, outbox: &Outbox) -> Result<(), CacheError> {
        self.write(&self.dir.join(OUTBOX), outbox)
    }

    fn write<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), CacheError> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir)?;
        let bytes = codec::encode_body(value)?;
        slopty_platform::fs::replace(path, &bytes)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    fn prune(&self) -> Result<(), CacheError> {
        let mut threads: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(&self.dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == THREAD_EXT))
            .filter_map(|p| Some((fs::metadata(&p).ok()?.modified().ok()?, p)))
            .collect();
        if threads.len() <= CACHE_THREADS {
            return Ok(());
        }
        threads.sort_unstable();
        for (_, path) in threads.iter().take(threads.len().saturating_sub(CACHE_THREADS)) {
            let _gone = fs::remove_file(path);
        }
        Ok(())
    }
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, CacheError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(codec::decode_body(&bytes)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

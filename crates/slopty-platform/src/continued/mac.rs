//! A download's progress on the file it lands as, where Finder draws it.
//!
//! Finder shows a progress published for a file URL (`NSProgress` of kind file, operation
//! downloading) as a pie on that item's icon, with a button that cancels it, as it does for
//! Safari's downloads. A download lands in a hidden directory beside its destination and is
//! renamed into place at the end, so until then Finder has no item to draw on: an empty
//! placeholder holds the destination's name, and goes when the work ends, before the rename.

use std::path::{Path, PathBuf};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::{
    NSProgress, NSProgressFileOperationKindDownloading, NSProgressKindFile, NSString, NSURL,
};

use super::Expired;

/// A progress published on a destination, unpublished when dropped.
#[derive(Debug)]
pub(super) struct OnFile {
    progress: Retained<NSProgress>,
    /// The placeholder made for it, when there was nothing there.
    placeholder: Option<PathBuf>,
}

impl OnFile {
    /// Publish a progress on `at`, making a placeholder there when nothing is; a cancel from
    /// Finder calls `expired`.
    pub(super) fn publish(at: &Path, expired: Expired) -> Self {
        let placeholder = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(at)
            .map(|_file| at.to_path_buf())
            .map_err(|e| tracing::debug!(at = %at.display(), error = %e, "no placeholder"))
            .ok();
        let progress = NSProgress::discreteProgressWithTotalUnitCount(-1);
        let url = NSURL::fileURLWithPath(&NSString::from_str(&at.to_string_lossy()));
        // SAFETY: Foundation rule: `NSProgressKindFile` and
        // `NSProgressFileOperationKindDownloading` are constant strings valid for the life of
        // the process.
        let (kind, operation) =
            unsafe { (NSProgressKindFile, NSProgressFileOperationKindDownloading) };
        progress.setKind(Some(kind));
        progress.setFileOperationKind(Some(operation));
        progress.setFileURL(Some(&url));
        progress.setCancellable(true);
        progress.setPausable(false);
        let cancelled = RcBlock::new(move || expired());
        // SAFETY: Foundation rule: the cancellation handler is copied when set and may be
        // called on any queue; the block holds only `Send + Sync` state (`Expired`).
        unsafe {
            progress.setCancellationHandler(Some(&cancelled));
        }
        progress.publish();
        Self { progress, placeholder }
    }

    /// `done` of `total` bytes are through.
    pub(super) fn progress(&self, done: u64, total: u64) {
        let count = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        self.progress.setTotalUnitCount(count(total));
        self.progress.setCompletedUnitCount(count(done.min(total)));
    }
}

impl Drop for OnFile {
    fn drop(&mut self) {
        self.progress.unpublish();
        // Only the placeholder as it was made: an empty file nothing has written to since.
        if let Some(path) = &self.placeholder
            && std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() == 0)
            && let Err(e) = std::fs::remove_file(path)
        {
            tracing::debug!(path = %path.display(), error = %e, "placeholder left");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Work;

    /// Something already at the destination is shown on and left alone. Finder's view of a
    /// published progress is `tests/main_thread.rs`, which runs on the main thread, where
    /// Foundation hands a publish to a subscriber.
    #[test]
    fn a_file_already_there_is_left_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().join("kept.txt");
        std::fs::write(&at, b"").unwrap();
        let work = Work::begin("Downloading kept.txt", "From the worker", Some(&at), || {});
        work.end(true);
        assert!(at.is_file(), "not a placeholder of the work's");
    }
}

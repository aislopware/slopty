//! A transfer kept running while the app is off screen, with the system's progress UI
//! (`slopty_platform::continued`): a Live Activity on iOS, and on macOS a download's progress on
//! the file it lands as, in Finder.
//!
//! The work begins where [`super::upload`] or [`super::download`] starts and ends where it
//! returns, across every relink in between. A person cancelling it from the Live Activity or
//! Finder cancels the transfer as the UI's cancel does, through its worker's line, on whichever
//! link it is by then or while it waits for one: its tasks stop and the worker is told while a
//! link is up.

use std::sync::{Arc, Weak};

use slopty_core::XferId;
use slopty_platform::continued::Work;

use super::{Entry, Line};

/// A transfer's work, ended as failed if the transfer's future is dropped before it ends it.
#[derive(Debug)]
pub(super) struct Offscreen(Arc<Work>);

impl Offscreen {
    /// The work, shared with whoever tells it the transfer's progress.
    pub(super) const fn work(&self) -> &Arc<Work> {
        &self.0
    }
}

impl Drop for Offscreen {
    fn drop(&mut self) {
        self.0.end(false);
    }
}

/// Begin the work of an upload of `list` as `xfer`, following `line`.
pub(super) fn upload(line: &Arc<Line>, xfer: XferId, list: &[Entry]) -> Offscreen {
    let (title, subtitle) = upload_titles(list);
    Offscreen(Arc::new(Work::begin(&title, &subtitle, None, cancel(line, xfer))))
}

/// Begin the work of a download of `path`, shown on the file `shown_at` when it lands as one;
/// `cancel` stops the download, on whichever link it is by then.
pub(super) fn download(
    path: &str,
    shown_at: Option<&std::path::Path>,
    cancel: impl Fn() + Send + Sync + 'static,
) -> Offscreen {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path);
    let title = format!("Downloading {name}");
    Offscreen(Arc::new(Work::begin(&title, "From the worker", shown_at, cancel)))
}

/// Cancel upload `xfer` on `line`, as the UI's cancel does.
fn cancel(line: &Arc<Line>, xfer: XferId) -> impl Fn() + Send + Sync + 'static {
    let line: Weak<Line> = Arc::downgrade(line);
    move || {
        let Some(line) = line.upgrade() else { return };
        tracing::info!(%xfer, "transfer cancelled from the system's progress");
        line.cancel(xfer);
    }
}

/// What the system's progress UI says of an upload: how many files, and which.
fn upload_titles(list: &[Entry]) -> (String, String) {
    let title = match list.len() {
        1 => "Uploading 1 file".to_owned(),
        n => format!("Uploading {n} files"),
    };
    let names: Vec<&str> =
        list.iter().take(2).map(|e| e.name.rsplit('/').next().unwrap_or(&e.name)).collect();
    let rest = list.len().saturating_sub(names.len());
    let subtitle = match (names.as_slice(), rest) {
        ([], _) => String::new(),
        ([one], _) => (*one).to_owned(),
        ([a, b], 0) => format!("{a} and {b}"),
        ([a, b], n) => format!("{a}, {b} and {n} more"),
        (more, _) => more.join(", "),
    };
    (title, subtitle)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use slopty_core::WallMs;

    use super::*;

    fn entry(name: &str) -> Entry {
        Entry {
            path: PathBuf::from("/tmp").join(name),
            name: name.to_owned(),
            size: 1,
            mtime_ms: WallMs::ZERO,
            mode: 0o644,
        }
    }

    /// The Live Activity names the files by their own names, two at most, and counts the rest.
    #[test]
    fn an_upload_is_titled_by_its_files() {
        let one = upload_titles(&[entry("notes.txt")]);
        assert_eq!(one, ("Uploading 1 file".to_owned(), "notes.txt".to_owned()));
        let two = upload_titles(&[entry("proj/a.rs"), entry("b.png")]);
        assert_eq!(two.1, "a.rs and b.png");
        let many = upload_titles(&[entry("a"), entry("b"), entry("c"), entry("d")]);
        assert_eq!(many, ("Uploading 4 files".to_owned(), "a, b and 2 more".to_owned()));
    }

    /// Cancelling an upload from the system's progress stops it through its worker's line,
    /// waiting for a link or not; once the line is gone it does nothing.
    #[test]
    fn a_cancel_from_the_system_stops_an_upload() {
        let line = Line::of(slopty_core::WorkerId::new());
        let xfer = XferId::new();
        let following = line.follow(xfer);
        let cancel = cancel(&line, xfer);
        cancel();
        assert!(*following.stop.borrow(), "the upload is told to stop");
        drop((following, line));
        cancel();
    }
}

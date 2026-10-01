//! A transfer kept running while the app is off screen, with the system's progress UI
//! (`slopty_platform::continued`): a Live Activity on iOS, and on macOS a download's progress on
//! the file it lands as, in Finder.
//!
//! The work begins where [`super::upload`] or [`super::download`] starts and ends where it
//! returns. A person cancelling it from the Live Activity or Finder cancels the transfer as the
//! UI's cancel does: its tasks stop at their next chunk and the worker is told.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use slopty_core::XferId;
use slopty_net::ClientMsg;
use slopty_platform::continued::Work;
use slopty_proto::transfer::XferMsg;
use tokio::sync::mpsc;

use super::{Entry, Table};

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

/// Begin the work of an upload of `list` as `xfer`.
pub(super) fn upload(
    table: &Arc<Table>,
    out: &mpsc::Sender<ClientMsg>,
    xfer: XferId,
    list: &[Entry],
) -> Offscreen {
    let (title, subtitle) = upload_titles(list);
    let current = Arc::new(Mutex::new(xfer));
    Offscreen(Arc::new(Work::begin(&title, &subtitle, None, cancel(table, out, xfer, current))))
}

/// Begin the work of a download of `path` as `xfer`, shown on the file `shown_at` when it
/// lands as one; `current` is its attempt now, which a retry moves on.
pub(super) fn download(
    table: &Arc<Table>,
    out: &mpsc::Sender<ClientMsg>,
    xfer: XferId,
    path: &str,
    shown_at: Option<&std::path::Path>,
    current: Arc<Mutex<XferId>>,
) -> Offscreen {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path);
    let title = format!("Downloading {name}");
    let cancel = cancel(table, out, xfer, current);
    Offscreen(Arc::new(Work::begin(&title, "From the worker", shown_at, cancel)))
}

/// Cancel transfer `xfer` and its attempt now, as the UI's cancel does.
fn cancel(
    table: &Arc<Table>,
    out: &mpsc::Sender<ClientMsg>,
    xfer: XferId,
    current: Arc<Mutex<XferId>>,
) -> impl Fn() + Send + Sync + 'static {
    let table: Weak<Table> = Arc::downgrade(table);
    let out = out.clone();
    move || {
        let Some(table) = table.upgrade() else { return };
        let now = *current.lock();
        tracing::info!(%xfer, %now, "transfer cancelled from the system's progress");
        table.cancel(xfer);
        table.cancel(now);
        if let Err(e) = out.try_send(ClientMsg::Xfer(XferMsg::Cancel { xfer: now })) {
            tracing::debug!(%now, error = %e, "cancel not sent");
        }
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

    /// Cancelling from the system's progress stops the attempt the download is on now and
    /// tells the worker, as the UI's cancel does; once the link's table is gone it does nothing.
    #[test]
    fn a_cancel_from_the_system_stops_the_current_attempt() {
        let table = Arc::new(Table::default());
        let (out, mut sent) = mpsc::channel(4);
        let (first, retry) = (XferId::new(), XferId::new());
        let current = Arc::new(Mutex::new(first));
        let mut done = table.expect_download(retry, PathBuf::from("/tmp"), Arc::default());
        let cancel = cancel(&table, &out, first, Arc::clone(&current));
        *current.lock() = retry;
        cancel();
        assert!(table.cancelled(first) && table.cancelled(retry));
        assert!(matches!(done.try_recv(), Ok(Err(super::super::XferError::Cancelled))));
        assert!(
            matches!(sent.try_recv(), Ok(ClientMsg::Xfer(XferMsg::Cancel { xfer })) if xfer == retry)
        );
        drop(table);
        cancel();
        assert!(sent.try_recv().is_err(), "nothing once the link is gone");
    }
}

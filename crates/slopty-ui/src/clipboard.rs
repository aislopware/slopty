//! The clipboard shared with the workers, as the UI uses it.
//!
//! The bookkeeping lives in `slopty_client::clip`: [`ClipSync`] keeps this client's pasteboard in
//! step with the workers (announcing, relaying one worker's copy to another, writing a worker's
//! offer as promises) and [`provider`] keeps those promises. What is here is the UI's side of it:
//! the paths of a worker's copied files ([`worker_file_paths`]). A shell's paste is the terminal's
//! own (`terminal::ClipPaste::of`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use slopty_client::clip::Fetched;
pub use slopty_client::clip::{
    ClipFiles, ClipSync, digest, file_url_path, file_url_paths, provider,
};
use slopty_client::remote::Remote;
use slopty_proto::transfer::RepRef;

/// The paths a worker's copied files have there, `None` once the copy is gone.
///
/// Each file's URL is fetched over `remote` (from memory when it came inline), waiting at most
/// `wait` for each. Blocks: never on the main thread.
#[must_use]
pub fn worker_file_paths(
    remote: &Arc<dyn Remote>,
    urls: &[RepRef],
    wait: Duration,
) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::with_capacity(urls.len());
    for url in urls {
        let Fetched::Data(bytes) = remote.clip_fetch(url, None, wait) else { return None };
        paths.extend(file_url_paths(&bytes));
    }
    Some(paths)
}

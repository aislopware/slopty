//! The clipboard shared with the workers, as the UI uses it.
//!
//! The bookkeeping lives in `slopty_client::clip`: [`ClipSync`] keeps this client's pasteboard in
//! step with the workers (announcing, relaying one worker's copy to another, writing a worker's
//! offer as promises) and [`provider`] keeps those promises. What is here is the UI's side of it:
//! a shell's paste as the terminal view takes it ([`shell_paste`]), and the paths of a worker's
//! copied files ([`worker_file_paths`]).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use slopty_client::clip::{
    ClipFiles, ClipSync, digest, file_url_path, file_url_paths, provider,
};
use slopty_client::clip::{Fetched, ShellPaste};
use slopty_client::layout::WorkerKey;
use slopty_client::remote::Remote;
use slopty_core::ClientId;
use slopty_proto::ClientMsg;
use slopty_proto::transfer::{ClipMsg, RepRef};

use crate::terminal::ClipPaste;

/// What a paste into a shell of `worker` finds on the clipboard, as the terminal view takes it.
pub fn shell_paste(sync: &mut ClipSync, worker: WorkerKey, me: ClientId) -> ClipPaste {
    match sync.shell_paste(worker, me) {
        ShellPaste::Text => ClipPaste::Text,
        ShellPaste::Files(files) => ClipPaste::Files(files),
        ShellPaste::Picture { offer } => {
            ClipPaste::Picture { offer: offer.map(|o| ClientMsg::Clip(ClipMsg::Offer(o))) }
        }
    }
}

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

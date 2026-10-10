//! Every transfer in flight, both ways, on one list.
//!
//! The files dropped on tiles going up ([`super::Upload`]) and the worker files coming down here
//! (`Download`), each with its machine, how far it got, how fast it goes and when it should be
//! done, and its cancel. The title bar's transfers popover shows the list
//! ([`WorkspaceView::transfer_rows`]).
//!
//! The transfers that still mean something to a new run of the app are kept in the client's
//! data directory ([`slopty_client::xfer::ledger`]): at the next launch each waits for its
//! worker, listed meanwhile, and goes on once the worker links, an upload from what the worker
//! holds of it.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context};
use slopty_client::layout::WorkerKey;
use slopty_client::remote::Remote;
use slopty_client::xfer::ledger::{Kept, Ledger, Way};
use slopty_client::xfer::{Brought, Versions};
use slopty_core::XferId;
use tokio::sync::watch;

use super::{Bringing, Upload, sent};
use crate::kit;
use crate::workspace::WorkspaceView;

/// How far back a transfer's rate looks: long enough to smooth a link's bursts, short enough to
/// follow one that slowed.
pub const PACE_WINDOW: Duration = Duration::from_secs(5);

/// How long a transfer runs before its rate and time left are said: the first second's figure
/// is mostly the handshake and the first window of the congestion controller.
pub const PACE_FROM: Duration = Duration::from_secs(1);

/// A download's progress is drawn at most this often: the bar's figures move four times a
/// second, which reads as live without drawing the window for every chunk.
const SEEN_EVERY: Duration = Duration::from_millis(250);

/// How fast a transfer goes: its progress over the last [`PACE_WINDOW`].
#[derive(Clone, Debug, Default)]
pub struct Pace {
    /// When each count was heard, oldest first.
    samples: VecDeque<(Instant, u64)>,
}

impl Pace {
    /// Nothing heard yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { samples: VecDeque::new() }
    }

    /// `done` bytes had landed at `at`.
    pub fn note(&mut self, at: Instant, done: u64) {
        self.samples.push_back((at, done));
        // The oldest kept is the window's start: one before it would stretch the window.
        while self.samples.len() > 2
            && self
                .samples
                .get(1)
                .is_some_and(|(t, _)| at.saturating_duration_since(*t) >= PACE_WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// Bytes a second from the window's start to `now`, once it spans [`PACE_FROM`]. Measured
    /// to `now` rather than to the last count, so a transfer that stalls is seen to slow.
    #[must_use]
    pub fn rate(&self, now: Instant) -> Option<f64> {
        let (since, first) = *self.samples.front()?;
        let (_, last) = *self.samples.back()?;
        let span = now.saturating_duration_since(since);
        if span < PACE_FROM {
            return None;
        }
        #[expect(clippy::cast_precision_loss, reason = "a rate for a label")]
        let moved = last.saturating_sub(first) as f64;
        Some(moved / span.as_secs_f64())
    }

    /// How long until `total` at the rate now, once there is one.
    #[must_use]
    pub fn left(&self, now: Instant, done: u64, total: u64) -> Option<Duration> {
        let rate = self.rate(now).filter(|r| *r >= 1.0)?;
        #[expect(clippy::cast_precision_loss, reason = "a time for a label")]
        let rest = total.saturating_sub(done) as f64;
        Duration::try_from_secs_f64(rest / rate).ok()
    }
}

/// What a transfer's line says of its progress: `42% · 3.1 MB/s · 12 s left`, the rate and the
/// time left once [`Pace`] has them.
#[must_use]
pub fn progress_words(done: u64, total: u64, pace: &Pace, now: Instant) -> String {
    let percent = done.saturating_mul(100).checked_div(total).unwrap_or(0).min(100);
    let mut parts = vec![format!("{percent}%")];
    if let Some(rate) = pace.rate(now) {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "bytes")]
        let rate = rate as u64;
        parts.push(format!("{}/s", kit::size_label(rate)));
    }
    if let Some(left) = pace.left(now, done, total) {
        parts.push(format!("{} left", kit::clock(left)));
    }
    parts.join(crate::workspace::rollup::META_SEPARATOR)
}

/// A worker's file or folder coming down to a place here.
#[derive(Debug)]
pub(in crate::workspace) struct Download {
    /// The worker it comes from.
    worker: WorkerKey,
    /// Its path there.
    source: String,
    /// Its name.
    name: String,
    /// Where it lands.
    dest: PathBuf,
    /// Why it comes down, for what the person is told when it ends.
    bringing: Bringing,
    /// How far it got.
    brought: Brought,
    /// How fast it goes.
    pace: Pace,
    /// The worker's remote it goes through, which its cancel reaches.
    via: Arc<dyn Remote>,
}

/// A download asked for: the worker's `source` to `dest` here, as transfer `xfer`.
#[derive(Clone, Debug)]
pub(in crate::workspace) struct Down {
    /// The worker it comes from.
    pub worker: WorkerKey,
    /// The transfer it is.
    pub xfer: XferId,
    /// Its path on the worker.
    pub source: String,
    /// Where it lands here.
    pub dest: PathBuf,
    /// The files an earlier run began, whose partial files it takes up.
    pub versions: Versions,
}

/// One line of the transfers popover.
#[derive(Clone, Debug, PartialEq)]
pub struct TransferRow {
    /// The transfer.
    pub xfer: XferId,
    /// Up to the worker, else down from it.
    pub up: bool,
    /// What goes: its name, or how many files.
    pub name: String,
    /// The worker at the other end.
    pub machine: String,
    /// How far, 0 to 1; `None` while it waits or its size is not known.
    pub fraction: Option<f32>,
    /// Bytes landed, of `total`.
    pub done: u64,
    /// Bytes in it; 0 while not known.
    pub total: u64,
    /// How it goes, in words: `42% · 3.1 MB/s · 12 s left`, `Waiting for studio`.
    pub words: String,
}

/// The downloads in flight, the ledger and its writer, and what an earlier run left.
#[derive(Debug, Default)]
pub struct Transfers {
    /// Downloads in flight.
    downloads: HashMap<XferId, Download>,
    /// Every kept transfer in flight, as last written.
    ledger: Ledger,
    /// Hands the ledger to the task that writes it, once the app said where.
    writer: Option<watch::Sender<Ledger>>,
    /// Kept by an earlier run of the app, waiting for their worker's link.
    waiting: Vec<Kept>,
}

impl WorkspaceView {
    /// Keep the transfers in flight in the ledger at `path`, and take up what an earlier run
    /// left there: each is listed at once and goes on when its worker links. Written off the
    /// main thread, the newest ledger only.
    pub fn set_transfer_ledger(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let ledger = Ledger::load(&path);
        self.transfers.waiting.clone_from(&ledger.kept);
        let (writer, mut written) = watch::channel(ledger.clone());
        self.transfers.ledger = ledger;
        cx.background_executor()
            .spawn(async move {
                while written.changed().await.is_ok() {
                    let ledger = written.borrow_and_update().clone();
                    if let Err(e) = ledger.save(&path) {
                        tracing::warn!(path = %path.display(), error = %e, "transfer ledger not written");
                    }
                }
            })
            .detach();
        self.transfers.writer = Some(writer);
        let linked: Vec<WorkerKey> =
            self.workers.iter().filter(|(_, w)| w.link.is_some()).map(|(k, _)| *k).collect();
        for key in linked {
            self.take_up_kept(key, cx);
        }
        cx.notify();
    }

    /// Keep `kept` in the ledger until it ends.
    pub(in crate::workspace) fn keep_transfer(&mut self, kept: Kept) {
        self.transfers.ledger.keep(kept);
        self.write_ledger();
    }

    /// Transfer `xfer` ended, however it did: the ledger lets it go.
    pub(in crate::workspace) fn transfer_over(&mut self, xfer: XferId) {
        if self.transfers.ledger.end(xfer) {
            self.write_ledger();
        }
        if !self.transfers_in_flight() {
            self.close_transfers();
        }
    }

    /// Whether any transfer is in flight or waits for its worker.
    #[must_use]
    pub fn transfers_in_flight(&self) -> bool {
        if !self.transfers.downloads.is_empty() {
            return true;
        }
        self.uploads.values().any(Upload::listed) || !self.transfers.waiting.is_empty()
    }

    fn write_ledger(&self) {
        if let Some(writer) = &self.transfers.writer {
            writer.send_replace(self.transfers.ledger.clone());
        }
    }

    /// `key` linked: what an earlier run left for it goes on, an upload from what the worker
    /// holds of it, a download to where it was going.
    pub(in crate::workspace) fn take_up_kept(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        let Some(remote) = self.remote(key) else { return };
        let (now, rest): (Vec<Kept>, Vec<Kept>) =
            std::mem::take(&mut self.transfers.waiting).into_iter().partition(|k| k.worker == key);
        self.transfers.waiting = rest;
        for kept in now {
            match kept.way {
                Way::Up { tile, files, dest, total, scratch } => {
                    tracing::info!(xfer = %kept.xfer, files = files.len(), "an upload taken up again");
                    let upload = Upload {
                        total,
                        scratch,
                        via: Some(Arc::clone(&remote)),
                        names: names_of(&files),
                        ..Upload::taken_up(tile, &dest)
                    };
                    self.uploads.insert(kept.xfer, upload);
                    remote.upload(kept.xfer, files, dest, true);
                }
                Way::Down { source, dest, versions } => {
                    tracing::info!(xfer = %kept.xfer, %source, "a download taken up again");
                    let down = Down { worker: key, xfer: kept.xfer, source, dest, versions };
                    self.bring_down(down, Bringing::Save, cx);
                }
            }
        }
        cx.notify();
    }

    /// Bring `down` down, listed while it goes and kept across a relaunch; the person is told
    /// as `bringing` says when it ends.
    pub(in crate::workspace) fn bring_down(
        &mut self,
        down: Down,
        bringing: Bringing,
        cx: &mut Context<Self>,
    ) {
        self.bring_down_holding(down, bringing, None, cx);
    }

    /// [`Self::bring_down`], with `held` kept until the download has stopped writing, however
    /// it ends (landed, failed, stopped, its link lost, or the view gone): a folder's security
    /// scope (`slopty_platform::file_drop::picker::Scoped`), which the transfer's staging
    /// directory there needs until it is cleaned away.
    pub(in crate::workspace) fn bring_down_holding(
        &mut self,
        down: Down,
        bringing: Bringing,
        held: Option<Box<dyn std::any::Any>>,
        cx: &mut Context<Self>,
    ) {
        let xfer = down.xfer;
        let Some(remote) = self.remote(down.worker) else {
            self.show_notice(format!("The machine is away; nothing was {}", bringing.done()), cx);
            self.transfer_over(xfer);
            return;
        };
        let (seen, heard) = watch::channel(Brought::default());
        self.download_began(&down, bringing, Arc::clone(&remote), heard, cx);
        let task = cx.background_spawn(async move {
            let Down { source, dest, versions, .. } = down;
            super::bring_down_seen(remote.as_ref(), &source, &dest, xfer, Some(seen), versions)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            drop(held);
            let _gone = this.update(cx, |this, cx| this.download_over(xfer, result, cx));
        })
        .detach();
    }

    /// `down` began: it is listed, kept in the ledger, and moves as `heard` says.
    pub(in crate::workspace) fn download_began(
        &mut self,
        down: &Down,
        bringing: Bringing,
        via: Arc<dyn Remote>,
        mut heard: watch::Receiver<Brought>,
        cx: &mut Context<Self>,
    ) {
        let Down { worker, xfer, source, dest, versions } = down.clone();
        let name = super::worker_name(&source).to_owned();
        // A drop's place may be another app's scratch, which nobody reads after this run.
        if bringing.kept() {
            let way = Way::Down { source: source.clone(), dest: dest.clone(), versions };
            self.keep_transfer(Kept { xfer, worker, way });
        }
        let at = cx.background_executor().now();
        let mut pace = Pace::new();
        pace.note(at, 0);
        let download = Download {
            worker,
            source,
            name,
            dest,
            bringing,
            brought: Brought::default(),
            pace,
            via,
        };
        self.transfers.downloads.insert(xfer, download);
        cx.notify();
        // Ends when the download does, which drops the sender.
        cx.spawn(async move |this, cx| {
            while heard.changed().await.is_ok() {
                let now = heard.borrow_and_update().clone();
                let moved = this.update(cx, |this, cx| {
                    let at = cx.background_executor().now();
                    let Some(d) = this.transfers.downloads.get_mut(&xfer) else { return };
                    d.pace.note(at, now.done);
                    // A file begun is kept with its version, so the next run resumes it.
                    let begun = (now.versions != d.brought.versions && d.bringing.kept())
                        .then(|| (d.worker, d.source.clone(), d.dest.clone()));
                    d.brought = now;
                    if let Some((worker, source, dest)) = begun {
                        let versions = d.brought.versions.clone();
                        let way = Way::Down { source, dest, versions };
                        this.keep_transfer(Kept { xfer, worker, way });
                    }
                    cx.notify();
                });
                if moved.is_err() {
                    break;
                }
                cx.background_executor().timer(SEEN_EVERY).await;
            }
        })
        .detach();
    }

    /// Download `xfer` ended with `result`: it leaves the list and the ledger, and the person
    /// is told, unless they stopped it.
    pub(in crate::workspace) fn download_over(
        &mut self,
        xfer: XferId,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let download = self.transfers.downloads.remove(&xfer);
        self.transfer_over(xfer);
        let Some(download) = download else { return };
        let Download { name, dest, bringing, .. } = download;
        let text = match (result, bringing) {
            (Ok(()), Bringing::Save) => Some(format!("Saved {}", super::tildes(&dest))),
            (Ok(()), Bringing::Files) => {
                let saved = dest.file_name().map_or(name, |n| n.to_string_lossy().into_owned());
                Some(format!("Saved {saved} in Files"))
            }
            // The drop's own window showed it land; the paste it came down for goes on, or
            // says why not.
            (Ok(()), Bringing::Drag) | (_, Bringing::Paste) => None,
            (Err(e), _) => Some(format!("{name} was not {}: {e}", bringing.done())),
        };
        if let Some(text) = text {
            self.show_notice(text, cx);
        }
        cx.notify();
    }

    /// Stop transfer `xfer`, whichever way it goes, or one that waits for its worker; what the
    /// worker holds of an upload stays there.
    ///
    /// A drop's files stop with the drop itself: the window lets go of it on the worker, whose
    /// button no longer waits for them, and the drop's other files stop too.
    pub fn cancel_transfer(&mut self, xfer: XferId, cx: &mut Context<Self>) {
        if let Some(upload) = self.uploads.get(&xfer) {
            match upload.drag {
                Some(drag) => self.cancel_drop(upload.tile, drag, cx),
                None => self.cancel_upload(xfer, cx),
            }
            return;
        }
        if let Some(download) = self.transfers.downloads.remove(&xfer) {
            tracing::info!(%xfer, "download cancelled");
            download.via.cancel(xfer);
        }
        self.transfers.waiting.retain(|k| k.xfer != xfer);
        self.transfer_over(xfer);
        cx.notify();
    }

    /// Every transfer in flight, for the popover: the uploads, the downloads, then what an
    /// earlier run left waiting for its worker; each group by name.
    #[must_use]
    pub fn transfer_rows(&self, now: Instant) -> Vec<TransferRow> {
        let machine = |key: WorkerKey| {
            self.workers.get(&key).map_or_else(|| "the machine".to_owned(), |w| w.name.clone())
        };
        let mut ups: Vec<TransferRow> = self
            .uploads
            .iter()
            .filter(|(_, u)| u.listed())
            .map(|(xfer, u)| {
                let machine = machine(u.tile.worker);
                let words = if u.away_since.is_some() {
                    format!("Waiting for {machine}")
                } else {
                    progress_words(u.done, u.total, &u.pace, now)
                };
                let fraction = u.away_since.is_none().then(|| u.fraction());
                // A drop says the window it lands in, as the person aimed it.
                let name = match self.item(u.tile).filter(|_| u.drag.is_some()) {
                    Some(item) => format!("{} into {}", sent(&u.names), self.tile_title(item)),
                    None => sent(&u.names),
                };
                let (done, total) = (u.done, u.total);
                TransferRow { xfer: *xfer, up: true, name, machine, fraction, done, total, words }
            })
            .collect();
        ups.sort_by(|a, b| a.name.cmp(&b.name).then(a.xfer.cmp(&b.xfer)));
        let mut downs: Vec<TransferRow> = self
            .transfers
            .downloads
            .iter()
            .map(|(xfer, d)| {
                let Brought { done, total, .. } = d.brought;
                #[expect(clippy::cast_precision_loss, reason = "a fraction for a bar")]
                let fraction = (total > 0).then(|| (done as f32 / total as f32).clamp(0.0, 1.0));
                let words = if total == 0 {
                    "Starting".to_owned()
                } else {
                    progress_words(done, total, &d.pace, now)
                };
                let (name, machine) = (d.name.clone(), machine(d.worker));
                TransferRow { xfer: *xfer, up: false, name, machine, fraction, done, total, words }
            })
            .collect();
        downs.sort_by(|a, b| a.name.cmp(&b.name).then(a.xfer.cmp(&b.xfer)));
        let waiting = self.transfers.waiting.iter().map(|k| {
            let machine = machine(k.worker);
            let (up, name, total) = match &k.way {
                Way::Up { files, total, .. } => (true, sent(&names_of(files)), *total),
                Way::Down { source, .. } => (false, super::worker_name(source).to_owned(), 0),
            };
            let words = format!("Waiting for {machine}");
            let (xfer, fraction, done) = (k.xfer, None, 0);
            TransferRow { xfer, up, name, machine, fraction, done, total, words }
        });
        ups.into_iter().chain(downs).chain(waiting).collect()
    }
}

/// The names of the top-level files and folders `files` sends, as they are here.
fn names_of(files: &[PathBuf]) -> Vec<String> {
    files.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rate is said once a second has passed, over the last five seconds, and falls while
    /// nothing lands; the time left follows from it.
    #[test]
    fn a_transfers_rate_follows_its_last_seconds() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut pace = Pace::default();
        pace.note(at(0), 0);
        pace.note(at(500), 1_000_000);
        assert_eq!(pace.rate(at(500)), None, "not yet a second");
        pace.note(at(1_000), 2_000_000);
        assert_eq!(pace.rate(at(1_000)), Some(2_000_000.0));
        assert_eq!(pace.left(at(1_000), 2_000_000, 10_000_000), Some(Duration::from_secs(4)));
        assert_eq!(pace.rate(at(2_000)), Some(1_000_000.0), "a stall slows it");

        for s in 2..=10 {
            pace.note(at(s * 1_000), 2_000_000 + (s - 1) * 500_000);
        }
        let rate = pace.rate(at(10_000)).unwrap();
        assert!((rate - 500_000.0).abs() < 1.0, "the last five seconds only: {rate}");
        let words = progress_words(6_500_000, 13_000_000, &pace, at(10_000));
        assert_eq!(words, "50% \u{b7} 488 KB/s \u{b7} 13 s left");
        assert_eq!(progress_words(0, 0, &Pace::default(), at(0)), "0%");
    }
}

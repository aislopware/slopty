//! Hot exit: every file tile's unsaved edit is kept on this device ([`slopty_client::unsaved`])
//! within moments of each change, so a quit, a crash or a dead battery loses none of it, and a
//! tile takes it back.
//!
//! A backup is one per file tile, keyed by the worker's item: two tiles on one file (two
//! clients opened it at the same moment, or ⌘Z brought a closed one back beside a new one)
//! each keep their own edit, and after a restart each goes back to its own tile. A tile's backup
//! goes once the tile is clean, or closed for good.
//!
//! A change is written at once, then no more often than every [`KEEP_EVERY`] while the typing
//! goes on (Zed's throttle, not VS Code's debounce, which waits as long as the typing lasts),
//! and a large edit less often still, at most [`KEEP_BYTES_PER_SEC`]. A tile brings on a pass
//! only when its [`Mark`] moves, never for a caret blink. A pass reads no text on the UI thread:
//! it compares each tile's [`Mark`] with the one last written, and takes the editor's rope,
//! shared, for the ones behind; the text is read out and written off the UI thread, one pass at
//! a time. A write counts once it has finished: one that failed is tried again on its own,
//! after [`RETRY_FIRST`] and then longer, and the flush as the app quits writes again whatever
//! is still on its way, or failed.
//!
//! A tile that goes while its edit is unsaved and not let go (another client removed it, its
//! worker was forgotten, or it was closed with a waiting program's save unresolved) leaves its
//! edit here, keyed by the item that went. The first snapshot of its worker after the app next
//! starts gives it a tile again: a clean tile on its file takes it, else a new tile is opened
//! for it, never one holding another edit. Those that wait more than [`OLD_AFTER`] for their
//! worker are told of at start, in a notice whose "Discard" lets them go; nothing is dropped
//! unasked.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{Context, Entity};
use slopty_client::layout::WorkerKey;
use slopty_client::unsaved::Store;
use slopty_core::{ItemId, WallMs};
use slopty_proto::handoff::EditOutcome;
use slopty_proto::items::{Item, ItemKind, ItemOp};

use super::WorkspaceView;
use super::toast::ToastKind;
use crate::file::{Backup, FileView, Mark};

/// How often, at most, the edits are written while they keep changing.
const KEEP_EVERY: Duration = Duration::from_millis(200);

/// The most the backups write a second while the typing goes on: past 1.6 MiB of edits a pass,
/// the passes space out beyond [`KEEP_EVERY`], so a 16 MiB file is kept every 2 s and not
/// rewritten five times a second (`docs/MEASUREMENTS.md`, "keeping an unsaved edit").
const KEEP_BYTES_PER_SEC: u64 = 8 << 20;

/// How long a write that failed waits to be tried again, the first time; each failure after
/// it doubles the wait, up to [`RETRY_MOST`].
const RETRY_FIRST: Duration = Duration::from_secs(1);

/// The longest a write that keeps failing waits to be tried again.
const RETRY_MOST: Duration = Duration::from_secs(30);

/// How many passes in a row may fail before the person is told their edits are not kept:
/// one is a hiccup, a few in a row (about seven seconds of retries) is a disk that is full or
/// will not be written.
const FAILS_SAID: u32 = 3;

/// What the person is told once [`FAILS_SAID`] passes in a row failed, with the first error.
pub(super) const NOT_KEPT: &str = "Unsaved edits can't be kept on this device";

/// A kept edit whose tile has not come back for this long is old: the person hears of it at
/// start, with the way to let it go.
const OLD_AFTER: Duration = Duration::from_hours(7 * 24);

/// One file tile on one worker: the worker's item, which outlives the app.
type TileKey = (WorkerKey, ItemId);

/// A kept edit no tile holds: read at start, or left by a tile that went.
struct Pending {
    backup: Backup,
    /// When it was last written: how long it has waited.
    kept_ms: WallMs,
}

/// What was last asked of a tile's backup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    /// Hold the edit at this mark.
    Put(Mark),
    /// Hold nothing.
    Drop,
}

/// The last change handed to the store for one tile.
#[derive(Clone, Copy, Debug)]
struct Last {
    seq: u64,
    op: Op,
    /// It finished. Until then (or when it failed) it is asked again.
    done: bool,
}

/// The backups and what the workspace knows of them.
pub(super) struct Kept {
    store: Store,
    /// Each tile's last change, while its backup may be there.
    last: HashMap<TileKey, Last>,
    /// Kept edits no tile holds.
    pending: HashMap<TileKey, Pending>,
    /// Tiles closed for good, or kept edits discarded or moved to another tile: the next pass
    /// lets their backup go.
    let_go: HashSet<TileKey>,
    /// A pass is writing, or waiting out [`KEEP_EVERY`] after one.
    busy: bool,
    /// Something changed since the last pass looked.
    stale: bool,
    /// The number of the last change handed to the store: of two changes to one backup, the
    /// later stands, whichever thread writes it first.
    seq: u64,
    /// The passes that failed in a row since the last that wrote everything.
    failures: u32,
}

impl Kept {
    const fn next(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    /// Hand the store change `op` to `key`.
    fn ask(&mut self, key: TileKey, op: Op) -> u64 {
        let seq = self.next();
        self.last.insert(key, Last { seq, op, done: false });
        seq
    }
}

/// What one pass does to the store, each change numbered.
enum Keep {
    /// Write the edit; kept at the time given, or now.
    Put(Backup, Option<WallMs>, u64),
    Drop(TileKey, u64),
}

/// The tile a backup follows, or the kept edit.
enum Source<'a> {
    Tile(&'a Entity<FileView>),
    Pending,
}

impl WorkspaceView {
    /// Keep the file tiles' unsaved edits in `store`, and take back the ones it holds from
    /// before, read off the UI thread: each is laid over its tile once that is made (or at
    /// once, on one already there).
    pub fn set_unsaved_store(&mut self, store: Store, cx: &Context<Self>) {
        let reading = store.clone();
        self.kept = Some(Kept {
            store,
            last: HashMap::new(),
            pending: HashMap::new(),
            let_go: HashSet::new(),
            busy: false,
            stale: false,
            seq: 0,
            failures: 0,
        });
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let all = reading.all();
                    all.iter().map(|u| (Backup::kept(u), u.kept_ms)).collect::<Vec<_>>()
                })
                .await;
            let _gone = this.update(cx, |this, cx| this.kept_found(found, cx));
        })
        .detach();
    }

    /// The backups from before are read: each goes to its own tile, or waits for it; a tile
    /// edited since keeps its own, later edit.
    fn kept_found(&mut self, found: Vec<(Backup, WallMs)>, cx: &mut Context<Self>) {
        if !found.is_empty() {
            tracing::info!(files = found.len(), "unsaved edits kept from before");
        }
        let now = WallMs::now();
        let mut old = 0_usize;
        for (backup, kept_ms) in found {
            let key = (backup.worker, backup.item);
            let view = self.files.get(&backup.item).filter(|v| v.read(cx).worker() == key.0);
            let view = view.cloned();
            let Some(kept) = self.kept.as_mut() else { return };
            let done = Last { seq: 0, op: Op::Put(backup.mark), done: true };
            kept.last.entry(key).or_insert(done);
            if let Some(view) = view {
                if view.read(cx).backup_mark().is_none() {
                    view.update(cx, |v, cx| v.restore(backup, cx));
                }
                continue;
            }
            if now.since(kept_ms) > OLD_AFTER {
                old = old.saturating_add(1);
            }
            kept.pending.insert(key, Pending { backup, kept_ms });
        }
        let synced: Vec<WorkerKey> = self
            .workers
            .iter()
            .filter(|(_, w)| w.link.is_some() && !w.awaiting_snapshot)
            .map(|(key, _)| *key)
            .collect();
        for key in synced {
            self.reopen_kept(key, cx);
        }
        if old > 0 {
            self.show_toast(ToastKind::OldUnsaved(old_notice(old)), cx);
        }
    }

    /// The edit kept for file tile `item` on `worker`, for its view now being made.
    pub(super) fn take_kept(&mut self, worker: WorkerKey, item: ItemId) -> Option<Backup> {
        let pending = self.kept.as_mut()?.pending.remove(&(worker, item))?;
        Some(pending.backup)
    }

    /// Give a tile to every edit kept on `key` whose own tile is no longer on the worker: it
    /// went while the edit was unsaved (another client closed it), and the edit is not to be
    /// lost. A clean tile on the same file takes it; otherwise a new tile is opened for it, so
    /// it never displaces another edit.
    pub(super) fn reopen_kept(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        let Some(kept) = &self.kept else { return };
        let Some(w) = self.workers.get(&key) else { return };
        let mut orphans: Vec<(ItemId, String)> = kept
            .pending
            .iter()
            .filter(|((k, item), _)| *k == key && w.doc.get(*item).is_none())
            .map(|((_, item), p)| (*item, p.backup.path.clone()))
            .collect();
        orphans.sort();
        let mut taken: Vec<ItemId> = Vec::new();
        for (gone, path) in orphans {
            let free = |id: ItemId| {
                !taken.contains(&id)
                    && self.kept.as_ref().is_none_or(|k| !k.pending.contains_key(&(key, id)))
                    && self.files.get(&id).is_none_or(|v| v.read(cx).backup_mark().is_none())
            };
            let clean = self.workers.get(&key).and_then(|w| {
                w.doc
                    .items()
                    .filter(|i| matches!(&i.kind, ItemKind::File { path: p } if *p == path))
                    .map(|i| i.id)
                    .find(|id| free(*id))
            });
            let to = clean.unwrap_or_else(|| {
                let item = Item {
                    id: ItemId::new(),
                    kind: ItemKind::File { path: path.clone() },
                    name: None,
                    facts: std::collections::BTreeMap::new(),
                };
                let id = item.id;
                self.propose(key, ItemOp::Add(item), cx);
                id
            });
            taken.push(to);
            tracing::info!(%path, adopted = clean.is_some(), "a tile again for an unsaved edit");
            let Some(kept) = self.kept.as_mut() else { return };
            let Some(mut pending) = kept.pending.remove(&(key, gone)) else { continue };
            pending.backup.item = to;
            // Written under its new tile before the old one is let go: a crash between leaves
            // two copies, never none (`apply` puts before it drops).
            kept.let_go.insert((key, gone));
            if let Some(view) = self.files.get(&to).cloned() {
                view.update(cx, |v, cx| v.restore(pending.backup, cx));
            } else {
                kept.pending.insert((key, to), pending);
            }
        }
        self.keep_unsaved(cx);
    }

    /// A file tile changed: write the edits soon.
    pub(super) fn keep_unsaved(&mut self, cx: &Context<Self>) {
        let Some(kept) = self.kept.as_mut() else { return };
        kept.stale = true;
        if kept.busy {
            return;
        }
        kept.busy = true;
        cx.spawn(async move |this, cx| {
            let mut retry = RETRY_FIRST;
            loop {
                let pass = this
                    .update(cx, |this, cx| {
                        let kept = this.kept.as_mut()?;
                        if !kept.stale {
                            kept.busy = false;
                            return None;
                        }
                        kept.stale = false;
                        Some((this.unsaved_pass(cx), this.kept.as_ref()?.store.clone()))
                    })
                    .ok()
                    .flatten();
                let Some((pass, store)) = pass else { return };
                // A pass with nothing to write (the caret moved) keeps no one waiting: the next
                // edit is written at once.
                if pass.is_empty() {
                    continue;
                }
                let applied =
                    cx.background_executor().spawn(async move { apply(&store, pass) }).await;
                let failed = applied.failed.is_some();
                let _gone = this.update(cx, |this, cx| {
                    this.kept_done(&applied.done);
                    this.pass_failed(applied.failed.as_deref(), cx);
                });
                let wait = if failed {
                    let wait = retry;
                    retry = retry.saturating_mul(2).min(RETRY_MOST);
                    wait
                } else {
                    retry = RETRY_FIRST;
                    keep_every(applied.bytes)
                };
                cx.background_executor().timer(wait).await;
            }
        })
        .detach();
    }

    /// A pass ended, failing for `why` or not. A failed one comes again on its own, since
    /// nothing else may change to bring it on and the edit is still only in its tile; the
    /// [`FAILS_SAID`]th in a row is said, once until a pass writes everything again.
    fn pass_failed(&mut self, why: Option<&str>, cx: &mut Context<Self>) {
        let Some(kept) = self.kept.as_mut() else { return };
        let Some(why) = why else {
            kept.failures = 0;
            return;
        };
        kept.stale = true;
        kept.failures = kept.failures.saturating_add(1);
        if kept.failures == FAILS_SAID {
            self.show_notice(format!("{NOT_KEPT}: {}", crate::kit::first_line(why)), cx);
        }
    }

    /// Write what is left now, on this thread, the ones still on their way included: the app
    /// is quitting.
    pub fn keep_unsaved_now(&mut self, cx: &gpui::App) {
        let Some(store) = self.kept.as_ref().map(|k| k.store.clone()) else { return };
        let pass = self.unsaved_pass(cx);
        let applied = apply(&store, pass);
        self.kept_done(&applied.done);
    }

    /// Changes the store finished: each counts unless a later one to its backup was asked since.
    fn kept_done(&mut self, done: &[(TileKey, u64)]) {
        let Some(kept) = self.kept.as_mut() else { return };
        for (key, seq) in done {
            let Some(last) = kept.last.get_mut(key) else { continue };
            if last.seq != *seq {
                continue;
            }
            if last.op == Op::Drop {
                kept.last.remove(key);
            } else {
                last.done = true;
            }
        }
    }

    /// File tile `view` closed for good: its backup goes, and no other tile's.
    pub(super) fn let_go_unsaved(&mut self, view: &Entity<FileView>, cx: &Context<Self>) {
        let Some(kept) = self.kept.as_mut() else { return };
        let v = view.read(cx);
        kept.let_go.insert((v.worker(), v.id()));
        self.keep_unsaved(cx);
    }

    /// File tile `view` went some other way than closed for good (another client removed it,
    /// its worker was forgotten, or it closed with a waiting program's save unresolved): the
    /// program waiting on it hears it was given up, and its edit stays kept for a tile on the
    /// file to take. A view still held by a closed tile has not gone.
    pub(super) fn file_tile_gone(&mut self, view: &Entity<FileView>, cx: &mut Context<Self>) {
        if self.closed.iter().any(|c| c.file.as_ref().is_some_and(|f| f == view)) {
            return;
        }
        let (worker, waiting) = {
            let v = view.read(cx);
            (v.worker(), v.waiting())
        };
        if let Some(id) = waiting {
            view.update(cx, |v, cx| v.set_waiting(None, cx));
            self.file_edited(worker, id, EditOutcome::Cancelled);
        }
        let Some(backup) = view.read(cx).backup(cx) else { return };
        let Some(kept) = self.kept.as_mut() else { return };
        tracing::info!(path = %backup.path, "a tile went with its edit unsaved: kept");
        let key = (backup.worker, backup.item);
        kept.pending.insert(key, Pending { backup, kept_ms: WallMs::now() });
        self.keep_unsaved(cx);
    }

    /// The start-up notice's "Discard": the kept edits no tile has taken for a week go, on
    /// the person's word.
    pub(super) fn discard_old_unsaved(&mut self, cx: &mut Context<Self>) {
        let Some(kept) = self.kept.as_mut() else { return };
        let now = WallMs::now();
        let old: Vec<TileKey> = kept
            .pending
            .iter()
            .filter(|(_, p)| now.since(p.kept_ms) > OLD_AFTER)
            .map(|(key, _)| *key)
            .collect();
        for key in &old {
            kept.pending.remove(key);
            kept.let_go.insert(*key);
        }
        tracing::info!(files = old.len(), "old unsaved edits discarded");
        let said = match old.len() {
            0 => "No unsaved edit is over a week old".to_owned(),
            1 => "Discarded an unsaved edit over a week old".to_owned(),
            n => format!("Discarded {n} unsaved edits over a week old"),
        };
        self.keep_unsaved(cx);
        self.show_notice(said, cx);
    }

    /// What a pass writes and removes. Per tile (closed ones ⌘Z can bring back included) and
    /// per kept edit: its edit, written unless the store already holds it; and when it is
    /// clean, its backup let go, if one may be there. A tile gone keeps its backup as a kept
    /// edit, unless it was closed for good. Every write comes before every removal, so an edit
    /// moved to another tile is never on disk under neither.
    fn unsaved_pass(&mut self, cx: &gpui::App) -> Vec<Keep> {
        let mut views: Vec<&Entity<FileView>> = self.files.values().collect();
        views.extend(self.closed.iter().filter_map(|c| c.file.as_ref()));
        let Some(kept) = self.kept.as_mut() else { return Vec::new() };
        let mut wanted: Wanted<'_> = HashMap::new();
        for view in views {
            let v = view.read(cx);
            let key = (v.worker(), v.id());
            offer(&mut wanted, key, v.backup_mark(), Source::Tile(view));
        }
        for (key, pending) in &kept.pending {
            offer(&mut wanted, *key, Some(pending.backup.mark), Source::Pending);
        }
        for key in kept.let_go.drain() {
            offer(&mut wanted, key, None, Source::Pending);
        }
        let (mut pass, mut drops) = (Vec::new(), Vec::new());
        for (key, want) in wanted {
            let last = kept.last.get(&key).copied();
            match want {
                Some((mark, source)) => {
                    if last.is_some_and(|l| l.done && l.op == Op::Put(mark)) {
                        continue;
                    }
                    let (backup, kept_ms) = match source {
                        Source::Tile(view) => (view.read(cx).backup(cx), None),
                        Source::Pending => {
                            let p = kept.pending.get(&key);
                            (p.map(|p| p.backup.clone()), p.map(|p| p.kept_ms))
                        }
                    };
                    let Some(backup) = backup else { continue };
                    let seq = kept.ask(key, Op::Put(mark));
                    pass.push(Keep::Put(backup, kept_ms, seq));
                }
                None if last.is_some() => {
                    let seq = kept.ask(key, Op::Drop);
                    drops.push(Keep::Drop(key, seq));
                }
                None => {}
            }
        }
        pass.append(&mut drops);
        pass
    }
}

/// Each tile a pass looks at, and its edit (from its view, or kept for it).
type Wanted<'a> = HashMap<TileKey, Option<(Mark, Source<'a>)>>;

/// `source` has `mark` of `key`'s edits (`None`: it is clean): it is the one to keep when it
/// was changed last.
fn offer<'a>(wanted: &mut Wanted<'a>, key: TileKey, mark: Option<Mark>, source: Source<'a>) {
    let slot = wanted.entry(key).or_default();
    if let Some(mark) = mark
        && slot.as_ref().is_none_or(|(best, _)| best.edit < mark.edit)
    {
        *slot = Some((mark, source));
    }
}

/// What the person hears at start of `n` kept edits over [`OLD_AFTER`] old.
fn old_notice(n: usize) -> String {
    if n == 1 {
        "An unsaved edit over a week old waits for its machine".to_owned()
    } else {
        format!("{n} unsaved edits over a week old wait for their machines")
    }
}

/// How long the passes wait after one that wrote `bytes`: [`KEEP_EVERY`], or longer for a large
/// edit, to hold the writes to [`KEEP_BYTES_PER_SEC`].
fn keep_every(bytes: u64) -> Duration {
    let for_bytes = Duration::from_millis(bytes.saturating_mul(1_000) / KEEP_BYTES_PER_SEC);
    KEEP_EVERY.max(for_bytes)
}

/// What one pass did.
struct Applied {
    /// The changes that finished.
    done: Vec<(TileKey, u64)>,
    /// A change failed: it is asked again by a later pass, the edit still in its tile. Why the
    /// first one did.
    failed: Option<String>,
    /// The bytes of edit text written.
    bytes: u64,
}

/// Carry out one pass on the store, off the UI thread. A write that fails is logged.
fn apply(store: &Store, pass: Vec<Keep>) -> Applied {
    let mut applied = Applied { done: Vec::with_capacity(pass.len()), failed: None, bytes: 0 };
    for keep in pass {
        let (key, seq, result) = match keep {
            Keep::Put(backup, kept_ms, seq) => {
                let unsaved = backup.unsaved(kept_ms.unwrap_or_else(WallMs::now));
                tracing::debug!(path = %unsaved.path, seq, "unsaved edit kept");
                let len = u64::try_from(unsaved.text.len()).unwrap_or(u64::MAX);
                applied.bytes = applied.bytes.saturating_add(len);
                let result = store.put(&unsaved, seq);
                ((unsaved.worker, unsaved.item), seq, result)
            }
            Keep::Drop((worker, item), seq) => {
                let result = store.remove(worker, item, seq);
                ((worker, item), seq, result)
            }
        };
        match result {
            Ok(true) => applied.done.push((key, seq)),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, item = %key.1, "unsaved edit not kept");
                applied.failed.get_or_insert_with(|| e.to_string());
            }
        }
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An edit of a usual size is kept every [`KEEP_EVERY`] while the typing goes on; a large
    /// one less often, so its writes stay near [`KEEP_BYTES_PER_SEC`].
    #[test]
    fn a_large_edit_is_kept_less_often() {
        assert_eq!(keep_every(0), KEEP_EVERY);
        assert_eq!(keep_every(64 << 10), KEEP_EVERY);
        assert_eq!(keep_every(1 << 20), KEEP_EVERY);
        assert_eq!(keep_every(16 << 20), Duration::from_secs(2));
        assert_eq!(keep_every(u64::MAX), Duration::from_millis(u64::MAX / KEEP_BYTES_PER_SEC));
    }
}

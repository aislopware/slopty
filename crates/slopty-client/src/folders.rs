//! A folder as a client holds it across its pages, and the changes it asks of the worker's
//! files.
//!
//! [`FolderPages`] asks for the next page of a folder past [`FOLDER_ENTRIES`] entries as the
//! person wants more, and asks again for as many as they had after the worker lists the folder
//! anew. [`FsOps`] numbers the folder ops sent, holds them until the worker answers so the tile
//! can show them at once, and [`sentence`] says how one went.

use std::collections::BTreeMap;
use std::path::Path;

use slopty_proto::folder::{After, FOLDER_ENTRIES, FsOp, FsOutcome, FsRefusal, Listing};
use slopty_proto::{ClientMsg, RequestId};

/// Entries a page holds at most, as a count of entries.
const PAGE: usize = FOLDER_ENTRIES as usize;

/// One folder's listing, its pages joined in order.
#[derive(Debug, Clone)]
pub struct FolderPages {
    /// The folder, as `ListFolder` named it.
    path: String,
    /// The pages so far, as one listing.
    listing: Option<Listing>,
    /// Entries the person wants listed: a page more each time they ask.
    wanted: usize,
    /// The page asked for and not yet answered.
    asked: Option<After>,
}

impl FolderPages {
    /// `path` (as `ListFolder` names it), with its first page wanted.
    #[must_use]
    pub const fn new(path: String) -> Self {
        Self { path, listing: None, wanted: PAGE, asked: None }
    }

    /// The pages so far, as one listing; `None` before the first.
    #[must_use]
    pub const fn listing(&self) -> Option<&Listing> {
        self.listing.as_ref()
    }

    /// The worker listed the folder (its answer to `ListFolder`, or a watch's relist): its first
    /// page, in place of every page before it. Returns the next page to ask for when the person
    /// had wanted more than one.
    pub fn listed(&mut self, listing: Listing) -> Option<ClientMsg> {
        self.listing = Some(listing);
        self.asked = None;
        self.next()
    }

    /// A page past the first came (`WorkerMsg::FolderPage`). One that is not the page asked for
    /// (a relist came between) is dropped. Returns the page after it when more are wanted.
    pub fn page(&mut self, after: &After, listing: Listing) -> Option<ClientMsg> {
        if self.asked.as_ref() != Some(after) {
            return None;
        }
        self.asked = None;
        match (&mut self.listing, listing) {
            (
                Some(Listing::Listed { entries, total, .. }),
                Listing::Listed { entries: more, total: now, .. },
            ) => {
                entries.extend(more);
                *total = now;
            }
            (held, other) => *held = Some(other),
        }
        self.next()
    }

    /// The person wants another page: the request for it, unless every entry is here or one
    /// is on its way.
    pub fn more(&mut self) -> Option<ClientMsg> {
        let held = self.held();
        self.wanted = self.wanted.max(held.saturating_add(PAGE));
        self.next()
    }

    /// The folder holds entries not listed yet.
    #[must_use]
    pub fn has_more(&self) -> bool {
        self.held() < self.total()
    }

    const fn held(&self) -> usize {
        match &self.listing {
            Some(Listing::Listed { entries, .. }) => entries.len(),
            _ => 0,
        }
    }

    fn total(&self) -> usize {
        match &self.listing {
            Some(Listing::Listed { total, .. }) => usize::try_from(*total).unwrap_or(usize::MAX),
            _ => 0,
        }
    }

    /// The request for the next page wanted, marked as asked.
    fn next(&mut self) -> Option<ClientMsg> {
        if self.asked.is_some() || self.held() >= self.wanted.min(self.total()) {
            return None;
        }
        let Some(Listing::Listed { entries, .. }) = &self.listing else { return None };
        let after = After::of(entries.last()?);
        self.asked = Some(after.clone());
        Some(ClientMsg::FolderPage { path: self.path.clone(), after })
    }
}

/// The folder ops this client sent, numbered, until the worker answers each.
#[derive(Debug, Default, Clone)]
pub struct FsOps {
    last: RequestId,
    pending: BTreeMap<RequestId, FsOp>,
}

impl FsOps {
    /// Number `op` and hold it until it is answered: the message that asks for it.
    pub fn ask(&mut self, op: FsOp) -> ClientMsg {
        self.last = self.last.wrapping_add(1);
        self.pending.insert(self.last, op.clone());
        ClientMsg::FsOp { request: self.last, op }
    }

    /// The ops on their way, oldest first, for the tile to show before the worker answers.
    pub fn pending(&self) -> impl Iterator<Item = &FsOp> {
        self.pending.values()
    }

    /// The worker answered `request` (`WorkerMsg::FsDone`): the op it answers, with how it
    /// went; `None` for a request this client did not send or already heard.
    pub fn done(&mut self, request: RequestId, outcome: FsOutcome) -> Option<(FsOp, FsOutcome)> {
        self.pending.remove(&request).map(|op| (op, outcome))
    }

    /// The link went: the ops whose answers it took with it. Each may or may not have been
    /// done; the folder's next listing says.
    pub fn lost(&mut self) -> Vec<FsOp> {
        std::mem::take(&mut self.pending).into_values().collect()
    }
}

/// How `op` went, in a sentence for the person.
#[must_use]
pub fn sentence(op: &FsOp, outcome: &FsOutcome) -> String {
    match outcome {
        FsOutcome::Done { .. } => done(op),
        FsOutcome::Refused(why) => refused(why),
        FsOutcome::Failed { error } => format!("Could not {}: {error}", verb(op)),
    }
}

/// What is said of `op` when the link to `worker` went before its answer came.
#[must_use]
pub fn unanswered(worker: &str, op: &FsOp) -> String {
    format!("{worker} went out of reach before it said whether it could {}", verb(op))
}

fn done(op: &FsOp) -> String {
    match op {
        FsOp::MakeDir { name, .. } => format!("Made folder “{name}”"),
        FsOp::Move { from, to } if parent(from) == parent(to) => {
            format!("Renamed to “{}”", name(to))
        }
        FsOp::Move { to, .. } => format!("Moved to “{}”", name(parent(to))),
        FsOp::Trash { path: was } => format!("Moved “{}” to the Trash", name(was)),
    }
}

fn refused(why: &FsRefusal) -> String {
    match why {
        FsRefusal::NotAbsolute { path } => format!("“{path}” is not a full path"),
        FsRefusal::BadName { name } => format!("“{name}” cannot be a file’s name"),
        FsRefusal::Protected { path } => format!("“{path}” holds too much to move or trash"),
        FsRefusal::Clash { path } => format!("Something named “{}” is already there", name(path)),
        FsRefusal::Missing { path } => format!("“{}” is not there", name(path)),
        FsRefusal::IntoItself => "A folder cannot go inside itself".to_owned(),
        FsRefusal::OtherVolume => "That is on another volume: copy it there instead".to_owned(),
        FsRefusal::NoTrash => "That volume has no Trash, so it was left where it is".to_owned(),
    }
}

fn verb(op: &FsOp) -> String {
    match op {
        FsOp::MakeDir { name, .. } => format!("make “{name}”"),
        FsOp::Move { from, .. } => format!("move “{}”", name(from)),
        FsOp::Trash { path } => format!("trash “{}”", name(path)),
    }
}

/// The last name of a worker's path, or the path when it has none.
fn name(path: &str) -> &str {
    Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path)
}

/// The folder a worker's path is in.
fn parent(path: &str) -> &str {
    Path::new(path).parent().and_then(Path::to_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::folder::FolderEntry;
    use slopty_proto::orchestration::FileKind;

    use super::*;

    fn entries(names: impl IntoIterator<Item = String>) -> Vec<FolderEntry> {
        names
            .into_iter()
            .map(|name| FolderEntry {
                name,
                kind: FileKind::File,
                link: false,
                hidden: false,
                size: 0,
                items: None,
                modified_ms: WallMs::ZERO,
            })
            .collect()
    }

    fn page(from: usize, count: usize, total: u32) -> Listing {
        Listing::Listed {
            dir: "/w/big".to_owned(),
            entries: entries((from..).take(count).map(|i| format!("f{i:05}"))),
            total,
        }
    }

    fn asked(msg: Option<ClientMsg>) -> After {
        match msg {
            Some(ClientMsg::FolderPage { path, after }) if path == "~/big" => after,
            other => panic!("{other:?}"),
        }
    }

    /// The first page alone until the person wants more; then each page asks after the last
    /// entry held, joins in order, and asks for no page past the end or twice at once.
    #[test]
    fn pages_join_in_order_as_the_person_wants_more() {
        let mut big = FolderPages::new("~/big".to_owned());
        assert!(big.listed(page(0, PAGE, 4500)).is_none(), "one page until more is wanted");
        assert!(big.has_more());
        let after = asked(big.more());
        assert_eq!(after.name, "f01999");
        assert!(big.more().is_none(), "one page on its way at a time");
        assert!(big.page(&after, page(PAGE, PAGE, 4500)).is_none(), "only the page wanted");
        let after = asked(big.more());
        assert!(big.page(&after, page(2 * PAGE, 500, 4500)).is_none());
        assert!(!big.has_more());
        assert!(big.more().is_none(), "nothing past the end");
        let Some(Listing::Listed { entries, total, .. }) = big.listing() else { panic!() };
        assert_eq!((entries.len(), *total), (4500, 4500));
        assert!(entries.windows(2).all(|w| w[0].name < w[1].name));
    }

    /// A relist (the folder changed) puts its first page in place of every page, and asks
    /// again for as many as the person had; a page asked before it is dropped when it comes.
    #[test]
    fn a_relist_asks_again_for_the_pages_the_person_had() {
        let mut big = FolderPages::new("~/big".to_owned());
        big.listed(page(0, PAGE, 4100));
        let stale = asked(big.more());
        let again = asked(big.listed(page(0, PAGE, 4101)));
        assert_eq!(again, stale, "the same entry ends the new first page");
        let mut other = stale;
        other.name = "older".to_owned();
        assert!(big.page(&other, page(PAGE, PAGE, 4101)).is_none(), "not the page asked");
        let Some(Listing::Listed { entries, .. }) = big.listing() else { panic!() };
        assert_eq!(entries.len(), PAGE);
        big.page(&again, page(PAGE, PAGE, 4101));
        let Some(Listing::Listed { entries, .. }) = big.listing() else { panic!() };
        assert_eq!(entries.len(), 2 * PAGE);
    }

    /// A folder that went while a page was on its way says so in place of the pages.
    #[test]
    fn a_folder_gone_between_pages_says_so() {
        let mut big = FolderPages::new("~/big".to_owned());
        big.listed(page(0, PAGE, 3000));
        let after = asked(big.more());
        let gone = Listing::Missing { error: "No such file or directory".to_owned() };
        assert!(big.page(&after, gone.clone()).is_none());
        assert_eq!(big.listing(), Some(&gone));
        assert!(!big.has_more());
    }

    /// Ops are numbered, held until their answer, and answered once; a lost link hands back
    /// the ones it took.
    #[test]
    fn ops_are_held_until_their_answer() {
        let mut ops = FsOps::default();
        let trash = FsOp::Trash { path: "/w/a".to_owned() };
        let ClientMsg::FsOp { request, .. } = ops.ask(trash.clone()) else { panic!() };
        let ClientMsg::FsOp { request: second, .. } =
            ops.ask(FsOp::MakeDir { parent: "/w".to_owned(), name: "n".to_owned() })
        else {
            panic!()
        };
        assert_ne!(request, second);
        assert_eq!(ops.pending().count(), 2);
        let outcome = FsOutcome::Done { path: "/t/a".to_owned() };
        assert_eq!(ops.done(request, outcome.clone()), Some((trash, outcome.clone())));
        assert_eq!(ops.done(request, outcome), None, "answered once");
        assert_eq!(ops.lost().len(), 1);
        assert_eq!(ops.pending().count(), 0);
    }

    #[test]
    fn each_outcome_reads_as_a_sentence() {
        let mv = |from: &str, to: &str| FsOp::Move { from: from.to_owned(), to: to.to_owned() };
        let done = |path: &str| FsOutcome::Done { path: path.to_owned() };
        assert_eq!(sentence(&mv("/w/a.txt", "/w/b.txt"), &done("/w/b.txt")), "Renamed to “b.txt”");
        assert_eq!(sentence(&mv("/w/a.txt", "/x/a.txt"), &done("/x/a.txt")), "Moved to “x”");
        let trash = FsOp::Trash { path: "/w/old".to_owned() };
        assert_eq!(sentence(&trash, &done("/t/old")), "Moved “old” to the Trash");
        let clash = FsOutcome::Refused(FsRefusal::Clash { path: "/w/b.txt".to_owned() });
        assert_eq!(
            sentence(&mv("/w/a", "/w/b.txt"), &clash),
            "Something named “b.txt” is already there"
        );
        let failed = FsOutcome::Failed { error: "Permission denied".to_owned() };
        assert_eq!(sentence(&trash, &failed), "Could not trash “old”: Permission denied");
    }
}

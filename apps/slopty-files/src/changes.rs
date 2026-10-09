//! What the system was told of a worker's folders, and what changed since.
//!
//! A replicated File Provider learns of changes only by asking the working set for those since
//! a sync anchor it was handed. The extension keeps the last listing of every folder it gave
//! the system and watches those folders on the worker; when a watched folder's listing comes
//! again, the difference goes into a log, each change under the next sequence number, and the
//! anchor is where in the log the system has read up to.
//!
//! An anchor carries the log's epoch, drawn fresh each time the extension starts, so one from
//! an earlier run, or one older than the oldest change kept, is expired: the system then lists
//! everything again, which is always right.

use std::collections::{HashMap, VecDeque};

use crate::item::Item;

/// The most changes kept; an anchor from before the oldest kept is expired.
pub const KEPT: usize = 10_000;

/// One change to tell the system.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Change {
    /// The item is new, or something the system shows of it moved.
    Updated(Item),
    /// The item, and everything under it, is gone.
    Deleted(String),
}

/// The anchor is from another run, or older than every change kept.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("the sync anchor has expired")]
pub struct Expired;

/// The folders the system was given and the changes since.
#[derive(Debug)]
pub struct Changes {
    epoch: [u8; 16],
    /// The number of the last change logged.
    seq: u64,
    /// The changes kept, oldest first, each with its number.
    log: VecDeque<(u64, Change)>,
    /// Every folder given, by identifier: its items, by name.
    folders: HashMap<String, HashMap<String, Item>>,
    /// The folders being given a page at a time: the items of their pages so far.
    paging: HashMap<String, Vec<Item>>,
}

impl Default for Changes {
    fn default() -> Self {
        Self {
            epoch: *uuid::Uuid::new_v4().as_bytes(),
            seq: 0,
            log: VecDeque::new(),
            folders: HashMap::new(),
            paging: HashMap::new(),
        }
    }
}

impl Changes {
    /// The anchor that stands for everything logged so far.
    #[must_use]
    pub fn anchor(&self) -> Vec<u8> {
        let mut anchor = self.epoch.to_vec();
        anchor.extend_from_slice(&self.seq.to_le_bytes());
        anchor
    }

    /// The folders the system was given, by identifier.
    pub fn folders(&self) -> impl Iterator<Item = &str> {
        self.folders.keys().map(String::as_str)
    }

    /// The folder `folder` lists `items` now. Its first listing is only kept, since the system
    /// was handed it whole; a later one logs what differs from the last. Whether anything was
    /// logged.
    pub fn listed(&mut self, folder: &str, items: Vec<Item>) -> bool {
        let now: HashMap<String, Item> =
            items.into_iter().map(|item| (item.name.clone(), item)).collect();
        let Some(before) = self.folders.get(folder) else {
            self.folders.insert(folder.to_owned(), now);
            return false;
        };
        let mut changed: Vec<Change> = now
            .values()
            .filter(|item| before.get(&item.name) != Some(*item))
            .cloned()
            .map(Change::Updated)
            .collect();
        let gone: Vec<String> = before
            .values()
            .filter(|item| !now.contains_key(&item.name))
            .map(|item| item.id.clone())
            .collect();
        self.folders.insert(folder.to_owned(), now);
        for id in gone {
            self.forget_under(&id);
            changed.push(Change::Deleted(id));
        }
        let any = !changed.is_empty();
        for change in changed {
            self.log(change);
        }
        any
    }

    /// A page of the folder `folder` the system was given (`crate::pages`): the `first` starts
    /// it anew, and the `last` makes the pages so far its listing, as [`Self::listed`] takes one.
    /// A page after a first never seen (an enumeration the extension did not start) adds
    /// nothing, as the folder's listing would be wanting. Whether anything was logged.
    pub fn page(&mut self, folder: &str, first: bool, items: Vec<Item>, last: bool) -> bool {
        let pages = if first {
            Some(items)
        } else {
            self.paging.remove(folder).map(|mut pages| {
                pages.extend(items);
                pages
            })
        };
        let Some(pages) = pages else { return false };
        if last {
            self.listed(folder, pages)
        } else {
            self.paging.insert(folder.to_owned(), pages);
            false
        }
    }

    /// The folder `folder` is gone, or is no longer a folder. Whether the system had it.
    pub fn gone(&mut self, folder: &str) -> bool {
        self.paging.remove(folder);
        if self.folders.remove(folder).is_none() {
            return false;
        }
        self.forget_under(folder);
        self.log(Change::Deleted(folder.to_owned()));
        true
    }

    /// The changes after `anchor`, and the anchor that stands for them read.
    ///
    /// # Errors
    ///
    /// [`Expired`] for an anchor of another run or older than every change kept.
    pub fn since(&self, anchor: &[u8]) -> Result<(Vec<Change>, Vec<u8>), Expired> {
        let (epoch, seq) = anchor.split_at_checked(16).ok_or(Expired)?;
        let seq = u64::from_le_bytes(seq.try_into().map_err(|_short| Expired)?);
        let oldest = self.log.front().map_or(self.seq, |(first, _)| first.saturating_sub(1));
        if epoch != self.epoch || seq > self.seq || seq < oldest {
            return Err(Expired);
        }
        let after = self.log.iter().filter(|(n, _)| *n > seq).map(|(_, c)| c.clone()).collect();
        Ok((after, self.anchor()))
    }

    fn log(&mut self, change: Change) {
        self.seq = self.seq.saturating_add(1);
        self.log.push_back((self.seq, change));
        while self.log.len() > KEPT {
            self.log.pop_front();
        }
    }

    /// Drop every folder kept under `id`, which is gone with it.
    fn forget_under(&mut self, id: &str) {
        let prefix = format!("{id}/");
        self.folders.retain(|folder, _| folder != id && !folder.starts_with(&prefix));
        self.paging.retain(|folder, _| folder != id && !folder.starts_with(&prefix));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(parent: &str, name: &str, size: u64) -> Item {
        Item {
            id: crate::item::child(parent, name).unwrap(),
            parent: parent.to_owned(),
            name: name.to_owned(),
            folder: false,
            size,
            modified_ms: 1,
            children: None,
            hidden: false,
        }
    }

    fn folder(parent: &str, name: &str) -> Item {
        Item { folder: true, ..file(parent, name, 0) }
    }

    /// A folder's first listing logs nothing; a later one logs what is new or moved and what
    /// went, and the anchor reads them once.
    #[test]
    fn a_listing_logs_what_differs_from_the_last() {
        let mut changes = Changes::default();
        let start = changes.anchor();
        assert!(!changes.listed("", vec![file("", "a.txt", 1), folder("", "src")]));
        assert_eq!(changes.since(&start).unwrap(), (Vec::new(), start.clone()));
        assert!(!changes.listed("", vec![file("", "a.txt", 1), folder("", "src")]), "the same");

        assert!(changes.listed("", vec![file("", "a.txt", 2), file("", "b.txt", 1)]));
        let (seen, read) = changes.since(&start).unwrap();
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(seen.contains(&Change::Updated(file("", "a.txt", 2))));
        assert!(seen.contains(&Change::Updated(file("", "b.txt", 1))));
        assert!(seen.contains(&Change::Deleted("src".to_owned())));
        assert_eq!(changes.since(&read).unwrap(), (Vec::new(), read));
    }

    /// A folder given a page at a time is listed once its last page comes, from all of them: a
    /// first page starts it again, and a page whose first was never seen adds nothing.
    #[test]
    fn a_folder_given_in_pages_is_listed_once_whole() {
        let mut changes = Changes::default();
        let start = changes.anchor();
        assert!(!changes.page("", true, vec![file("", "a.txt", 1)], false));
        assert_eq!(changes.folders().count(), 0, "not given until its last page");
        assert!(!changes.page("", false, vec![file("", "b.txt", 1)], true));
        assert_eq!(changes.folders().collect::<Vec<_>>(), [""]);
        assert_eq!(changes.since(&start).unwrap().0, [], "its first listing logs nothing");

        assert!(!changes.page("", true, vec![file("", "a.txt", 1)], false));
        assert!(!changes.page("", true, vec![file("", "a.txt", 1)], false), "started again");
        assert!(changes.page("", false, vec![file("", "c.txt", 1)], true));
        let (seen, _) = changes.since(&start).unwrap();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(seen.contains(&Change::Updated(file("", "c.txt", 1))));
        assert!(seen.contains(&Change::Deleted("b.txt".to_owned())), "against both pages before");

        assert!(!changes.page("src", false, vec![file("src", "x", 1)], true), "no first page");
        assert!(!changes.folders().any(|f| f == "src"));
    }

    /// A folder that goes takes the folders kept under it, which log nothing more of their own,
    /// and a folder never given is not news.
    #[test]
    fn a_folder_that_goes_takes_what_was_under_it() {
        let mut changes = Changes::default();
        changes.listed("", vec![folder("", "src")]);
        changes.listed("src", vec![folder("src", "bin")]);
        changes.listed("src/bin", vec![file("src/bin", "main.rs", 3)]);
        changes.listed("srcs", vec![]);
        let start = changes.anchor();
        assert!(changes.gone("src"));
        let mut kept: Vec<&str> = changes.folders().collect();
        kept.sort_unstable();
        assert_eq!(kept, ["", "srcs"], "a sibling that starts the same stays");
        assert_eq!(changes.since(&start).unwrap().0, [Change::Deleted("src".to_owned())]);
        assert!(!changes.gone("src/bin"), "went with its folder");
        assert!(!changes.gone("never"));
    }

    /// An anchor of another run, one from the future, one that is no anchor, and one older
    /// than every change kept are expired; the oldest kept still reads.
    #[test]
    fn an_anchor_from_elsewhere_or_too_old_is_expired() {
        let mut changes = Changes::default();
        let start = changes.anchor();
        assert_eq!(Changes::default().since(&start), Err(Expired), "another run");
        assert_eq!(changes.since(b"short"), Err(Expired));
        let mut ahead = start.clone();
        ahead[16] = 9;
        assert_eq!(changes.since(&ahead), Err(Expired), "from the future");

        changes.listed("", vec![]);
        for n in 0..=KEPT {
            changes.listed("", vec![file("", "a", u64::try_from(n).unwrap())]);
        }
        assert_eq!(changes.since(&start), Err(Expired), "older than every change kept");
        let mut oldest = start;
        oldest[16..].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(changes.since(&oldest).unwrap().0.len(), KEPT);
    }
}

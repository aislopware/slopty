//! Who wrote each line of the files shown here, as the worker last said, kept until the file
//! changes.
//!
//! A tile that shows a file names it as it was read ([`Stamp`]): a file tile by its
//! modification time, a review by the blob its diff ends at. The answer the worker gives
//! ([`Authors`]) carries both, so one that matches is drawn at once on every hover and every
//! frame, and the worker is asked again only for a file that has moved on, once.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use slopty_core::WallMs;
use slopty_proto::thread::ThreadId;
use slopty_proto::thread::wire::{AuthorRun, Authors};

/// The files whose answers are kept at most, the least recently drawn going first.
pub const AUTHORS_KEPT: usize = 64;

/// A file as a tile read it, so an answer about it can be told current.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Stamp {
    /// Read whole, last changed then.
    Modified(WallMs),
    /// The blob git gives it.
    Blob(String),
}

impl Stamp {
    /// Whether `authors` is of the file as this read it.
    #[must_use]
    pub fn matches(&self, authors: &Authors) -> bool {
        match self {
            Self::Modified(at) => authors.modified_ms == Some(*at),
            Self::Blob(blob) => authors.blob.as_deref() == Some(blob.as_str()),
        }
    }
}

/// Which file an answer is about: as asked.
type Key = (Option<ThreadId>, String);

/// The answers held, by file.
#[derive(Clone, Debug, Default)]
pub struct Authorships {
    /// Least recently drawn first; each file once.
    held: VecDeque<(Key, Arc<Authors>)>,
    /// Files asked about and not answered, with the stamp they were asked at.
    asked: HashSet<(Key, Stamp)>,
}

impl Authorships {
    /// What is known of `path` as `stamp` read it, made the most recent.
    pub fn get(
        &mut self,
        thread: Option<ThreadId>,
        path: &str,
        stamp: &Stamp,
    ) -> Option<Arc<Authors>> {
        let at = self
            .held
            .iter()
            .position(|((t, p), a)| *t == thread && p == path && stamp.matches(a))?;
        let entry = self.held.remove(at)?;
        let found = Arc::clone(&entry.1);
        self.held.push_back(entry);
        Some(found)
    }

    /// Whether `path` as `stamp` read it is to be asked of the worker: nothing current is held,
    /// and it has not been asked already. Marks it asked.
    pub fn ask(&mut self, thread: Option<ThreadId>, path: &str, stamp: &Stamp) -> bool {
        let current =
            self.held.iter().any(|((t, p), a)| *t == thread && p == path && stamp.matches(a));
        !current && self.asked.insert(((thread, path.to_owned()), stamp.clone()))
    }

    /// Keep the worker's answer, in place of what was held of the file.
    pub fn heard(&mut self, authors: Authors) {
        let key = (authors.thread, authors.path.clone());
        self.asked.retain(|(k, stamp)| !(*k == key && stamp.matches(&authors)));
        self.held.retain(|(k, _)| *k != key);
        self.held.push_back((key, Arc::new(authors)));
        while self.held.len() > AUTHORS_KEPT {
            self.held.pop_front();
        }
    }

    /// The link went: what was asked is lost with it, and is asked again when drawn.
    pub fn forget_asked(&mut self) {
        self.asked.clear();
    }
}

/// The run that holds line `line` (from 1), if a thread wrote it.
#[must_use]
pub fn run_at(authors: &Authors, line: u32) -> Option<&AuthorRun> {
    let at = authors.runs.partition_point(|r| r.start.saturating_add(r.lines) <= line);
    authors.runs.get(at).filter(|r| r.start <= line)
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::wire::{AuthorRun, Authors};
    use slopty_proto::thread::{ThreadId, TurnId};

    use super::{Authorships, Stamp, run_at};

    fn answer(modified: u64, runs: Vec<AuthorRun>) -> Authors {
        Authors {
            thread: None,
            path: "/r/a.rs".to_owned(),
            modified_ms: Some(WallMs::from_millis(modified)),
            blob: Some(format!("blob{modified}")),
            runs,
            absent: None,
        }
    }

    fn run(start: u32, lines: u32) -> AuthorRun {
        AuthorRun {
            start,
            lines,
            thread: ThreadId::new(),
            turn: Some(TurnId(1)),
            commit: None,
            at_ms: WallMs::ZERO,
        }
    }

    /// A file is asked once; the answer is drawn from then with no ask, by either stamp; once
    /// the file moves on it is asked once more, and the answer for the old read is not drawn.
    #[test]
    fn a_file_is_asked_once_per_change() {
        let mut held = Authorships::default();
        let first = Stamp::Modified(WallMs::from_millis(10));
        assert!(held.ask(None, "/r/a.rs", &first));
        assert!(!held.ask(None, "/r/a.rs", &first), "asked already");
        held.heard(answer(10, vec![run(1, 2)]));
        assert!(!held.ask(None, "/r/a.rs", &first));
        assert!(held.get(None, "/r/a.rs", &first).is_some());
        assert!(held.get(None, "/r/a.rs", &Stamp::Blob("blob10".to_owned())).is_some());
        let later = Stamp::Modified(WallMs::from_millis(20));
        assert!(held.get(None, "/r/a.rs", &later).is_none());
        assert!(held.ask(None, "/r/a.rs", &later));
        held.forget_asked();
        assert!(held.ask(None, "/r/a.rs", &later), "the link went: asked again");
    }

    /// Only so many files are kept, the least recently drawn going first.
    #[test]
    fn the_least_recently_drawn_go_first() {
        let mut held = Authorships::default();
        for n in 0..super::AUTHORS_KEPT {
            let mut a = answer(1, Vec::new());
            a.path = format!("/r/{n}");
            held.heard(a);
        }
        let stamp = Stamp::Modified(WallMs::from_millis(1));
        assert!(held.get(None, "/r/0", &stamp).is_some());
        let mut more = answer(1, Vec::new());
        more.path = "/r/more".to_owned();
        held.heard(more);
        assert!(held.get(None, "/r/0", &stamp).is_some(), "drawn lately, so kept");
        assert!(held.get(None, "/r/1", &stamp).is_none());
    }

    #[test]
    fn a_line_finds_its_run() {
        let a = answer(1, vec![run(2, 3), run(9, 1)]);
        assert_eq!(run_at(&a, 1).map(|r| r.start), None);
        assert_eq!(run_at(&a, 2).map(|r| r.start), Some(2));
        assert_eq!(run_at(&a, 4).map(|r| r.start), Some(2));
        assert_eq!(run_at(&a, 5).map(|r| r.start), None);
        assert_eq!(run_at(&a, 9).map(|r| r.start), Some(9));
        assert_eq!(run_at(&a, 10).map(|r| r.start), None);
    }
}

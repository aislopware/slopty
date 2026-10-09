//! A folder of more than one listing's worth of entries, a page at a time.
//!
//! The worker lists at most [`FOLDER_ENTRIES`] entries of a folder at once and counts the rest
//! (`slopty_proto::folder`): a page past the first is asked for after the last entry the one
//! before held ([`After`]), so an entry added or removed meanwhile neither repeats nor is
//! skipped. The system enumerates a folder the same way, page by page, each page carried as
//! bytes the extension hands it and gets back when it asks for the next: a [`Cursor`] is the
//! worker's [`After`] in those bytes, so the extension keeps nothing between pages, nor across
//! its launches. [`Gather`] joins the pages of a whole folder, for what needs all of it at once:
//! an item found by its name, and the folder's listing that its changes are told against.

use slopty_proto::folder::{After, FOLDER_ENTRIES, Listing};

/// What marks a page as one of the extension's, apart from the system's own first pages
/// (`NSFileProviderInitialPageSortedByName` and `…ByDate`).
const MARK: &[u8; 4] = b"SLPG";

/// The most bytes a page may be: a larger one ends the enumeration.
pub const PAGE_BYTES: usize = 500;

/// Where the system's next page of a folder starts.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cursor {
    /// How many of the folder's entries the pages before it held.
    pub held: u32,
    /// The last entry the page before held.
    pub after: After,
}

impl Cursor {
    /// The page's bytes, at most [`PAGE_BYTES`].
    ///
    /// A name too long to fit (more than any disk but HFS+ holds) is cut short at a character:
    /// it sorts before the whole name, so the next page starts a little early and repeats an
    /// entry or a few the system already has, which only tells it them again, and skips none.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut page = Vec::with_capacity(PAGE_BYTES);
        page.extend_from_slice(MARK);
        page.extend_from_slice(&self.held.to_le_bytes());
        page.push(u8::from(self.after.folder));
        let room = PAGE_BYTES.saturating_sub(page.len());
        let name = &self.after.name;
        let cut = (0..=room.min(name.len())).rev().find(|at| name.is_char_boundary(*at));
        page.extend_from_slice(name.get(..cut.unwrap_or(0)).unwrap_or_default().as_bytes());
        page
    }

    /// The cursor `page` holds; `None` for one of the system's first pages, or any page that
    /// is not the extension's.
    #[must_use]
    pub fn decode(page: &[u8]) -> Option<Self> {
        let rest = page.strip_prefix(MARK)?;
        let (held, rest) = rest.split_first_chunk::<4>()?;
        let (folder, name) = rest.split_first()?;
        let folder = match folder {
            0 => false,
            1 => true,
            _ => return None,
        };
        let name = std::str::from_utf8(name).ok()?.to_owned();
        Some(Self { held: u32::from_le_bytes(*held), after: After { folder, name } })
    }

    /// The page after one of `listing` that came after `held` entries; `None` once the folder
    /// is all told. A full page asks for one more even when the count says it was the last,
    /// so an entry added meanwhile is listed; an empty page is the end.
    #[must_use]
    pub fn next(held: u32, listing: &Listing) -> Option<Self> {
        let Listing::Listed { entries, total, .. } = listing else { return None };
        let last = entries.last()?;
        let count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
        let held = held.saturating_add(count);
        (held < *total || count >= FOLDER_ENTRIES).then(|| Self { held, after: After::of(last) })
    }
}

/// A whole folder's listing, joined page by page.
#[derive(Clone, Debug)]
pub struct Gather {
    listing: Listing,
    /// The last page held nothing: the folder is all told whatever its count says.
    ended: bool,
}

impl Gather {
    /// From the folder's first page.
    #[must_use]
    pub const fn new(first: Listing) -> Self {
        Self { listing: first, ended: false }
    }

    /// The page to ask for next; `None` once the folder is whole, or is no folder.
    #[must_use]
    pub fn wants(&self) -> Option<After> {
        if self.ended {
            return None;
        }
        let Listing::Listed { entries, total, .. } = &self.listing else { return None };
        let held = u32::try_from(entries.len()).unwrap_or(u32::MAX);
        (held < *total).then(|| entries.last().map(After::of)).flatten()
    }

    /// The page after [`Self::wants`] came: its entries join the rest. One that finds no folder
    /// there any more (removed, or a file in its place) is the folder's listing now.
    pub fn add(&mut self, page: Listing) {
        match (&mut self.listing, page) {
            (
                Listing::Listed { entries, total, .. },
                Listing::Listed { entries: more, total: now, .. },
            ) => {
                self.ended = more.is_empty();
                entries.extend(more);
                *total = now;
            }
            (held, other) => *held = other,
        }
    }

    /// The listing joined so far.
    #[must_use]
    pub fn listing(self) -> Listing {
        self.listing
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::folder::FolderEntry;
    use slopty_proto::orchestration::FileKind;

    use super::*;

    /// A worker's folder as it pages it: its entries in its order (folders first, then by name
    /// in any case), each page the [`FOLDER_ENTRIES`] after the entry asked after.
    struct Fake {
        entries: Vec<FolderEntry>,
    }

    fn entry(name: &str, folder: bool) -> FolderEntry {
        FolderEntry {
            name: name.to_owned(),
            kind: if folder { FileKind::Dir } else { FileKind::File },
            link: false,
            hidden: false,
            size: 1,
            items: None,
            modified_ms: WallMs::from_millis(0),
        }
    }

    fn key(folder: bool, name: &str) -> (bool, String, String) {
        (!folder, name.to_lowercase(), name.to_owned())
    }

    impl Fake {
        /// `folders` folders and `files` files.
        fn new(folders: usize, files: usize) -> Self {
            let mut fake = Self { entries: Vec::new() };
            for n in 0..folders {
                fake.add(&format!("dir{n:05}"), true);
            }
            for n in 0..files {
                fake.add(&format!("file{n:05}.txt"), false);
            }
            fake
        }

        fn add(&mut self, name: &str, folder: bool) {
            self.entries.push(entry(name, folder));
            self.entries.sort_by_key(|e| key(e.kind == FileKind::Dir, &e.name));
        }

        fn remove(&mut self, name: &str) {
            self.entries.retain(|e| e.name != name);
        }

        /// The page after `after`, or the first.
        fn page(&self, after: Option<&After>) -> Listing {
            let from = after.map(|a| key(a.folder, &a.name));
            let keep = usize::try_from(FOLDER_ENTRIES).unwrap_or(usize::MAX);
            let entries = self
                .entries
                .iter()
                .filter(|e| {
                    from.as_ref().is_none_or(|f| key(e.kind == FileKind::Dir, &e.name) > *f)
                })
                .take(keep)
                .cloned()
                .collect();
            let total = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
            Listing::Listed { dir: "/home/me/big".to_owned(), entries, total }
        }
    }

    fn names(listing: &Listing) -> Vec<String> {
        match listing {
            Listing::Listed { entries, .. } => entries.iter().map(|e| e.name.clone()).collect(),
            other => panic!("{other:?}"),
        }
    }

    /// Walk `fake` as the system does: the first page, then each from the bytes the page before
    /// handed it, `between` running after each page. The names told, page by page.
    fn walk(fake: &mut Fake, mut between: impl FnMut(&mut Fake, usize)) -> Vec<Vec<String>> {
        let mut told = Vec::new();
        let mut page: Option<Vec<u8>> = None;
        loop {
            let cursor = page.as_deref().and_then(Cursor::decode);
            let listing = fake.page(cursor.as_ref().map(|c| &c.after));
            told.push(names(&listing));
            between(fake, told.len());
            let held = cursor.map_or(0, |c| c.held);
            let Some(next) = Cursor::next(held, &listing) else { return told };
            let bytes = next.encode();
            assert!(bytes.len() <= PAGE_BYTES, "{} bytes", bytes.len());
            page = Some(bytes);
            assert!(told.len() < 100, "the walk never ends");
        }
    }

    /// A folder of 4500 entries is told in three pages, folders first and each entry once; a
    /// folder of exactly one page asks once more and ends on the empty page; a small one is a
    /// single page.
    #[test]
    fn a_big_folder_is_told_page_by_page_each_entry_once() {
        let mut fake = Fake::new(300, 4200);
        let pages = walk(&mut fake, |_, _| {});
        let sizes: Vec<usize> = pages.iter().map(Vec::len).collect();
        assert_eq!(sizes, [2000, 2000, 500]);
        let told: Vec<String> = pages.concat();
        let all: Vec<String> = fake.entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(told, all, "in the worker's order, none twice");
        assert_eq!(told.first().map(String::as_str), Some("dir00000"));

        let mut full = Fake::new(0, 2000);
        let sizes: Vec<usize> = walk(&mut full, |_, _| {}).iter().map(Vec::len).collect();
        assert_eq!(sizes, [2000, 0], "a full page asks once more");

        let mut small = Fake::new(2, 3);
        assert_eq!(walk(&mut small, |_, _| {}).len(), 1, "one page");
    }

    /// Entries added and removed between pages: one added before the pages told so far is not
    /// told (the working set's changes tell it), one added ahead is told once, one removed ahead
    /// is not, and none told is told again.
    #[test]
    fn a_folder_that_changes_between_pages_repeats_and_skips_nothing_ahead() {
        let mut fake = Fake::new(0, 4500);
        let pages = walk(&mut fake, |fake, page| {
            if page == 1 {
                fake.add("file00000a.txt", false);
                fake.add("file04400a.txt", false);
                fake.remove("file04401.txt");
            }
        });
        let told: Vec<String> = pages.concat();
        let mut once = told.clone();
        once.sort();
        once.dedup();
        assert_eq!(once.len(), told.len(), "none told twice");
        assert!(!told.contains(&"file00000a.txt".to_owned()), "behind the pages told");
        assert!(told.contains(&"file04400a.txt".to_owned()), "ahead");
        assert!(!told.contains(&"file04401.txt".to_owned()), "gone before its page");
        assert_eq!(told.len(), 4500);
    }

    /// The page's bytes hold the cursor; the system's own first pages, and anything else, are
    /// none of the extension's. A name too long for the bytes is cut at a character, and the
    /// page after it starts no later than the entry, so nothing is skipped.
    #[test]
    fn a_cursor_is_its_bytes_and_a_long_name_is_cut_without_skipping() {
        let cursor = Cursor { held: 2000, after: After { folder: true, name: "Ñame".to_owned() } };
        assert_eq!(Cursor::decode(&cursor.encode()), Some(cursor));
        for system in [&[0_u8; 8][..], b"", b"SLPG", b"SLPG\x01\x00\x00\x00\x07x"] {
            assert_eq!(Cursor::decode(system), None, "{system:?}");
        }

        let long = "é".repeat(300);
        let mut fake = Fake::new(0, 1999);
        fake.add(&long, false);
        fake.add(&format!("{long}z"), false);
        let pages = walk(&mut fake, |_, _| {});
        let told: Vec<String> = pages.concat();
        assert!(told.contains(&format!("{long}z")), "the entry after a cut name is told");
        let cut = Cursor { held: 0, after: After { folder: false, name: long } }.encode();
        assert!(cut.len() <= PAGE_BYTES);
        let decoded = Cursor::decode(&cut).expect("ours");
        assert!(decoded.after.name.chars().all(|c| c == 'é'), "cut at a character");
    }

    /// The pages of a folder joined whole, asking after the last entry held until the count is
    /// reached; an empty page ends it whatever the count says, and a page that finds no folder
    /// is the listing.
    #[test]
    fn a_folder_gathered_whole_from_its_pages() {
        let fake = Fake::new(10, 4990);
        let mut gather = Gather::new(fake.page(None));
        let mut asked = 0;
        while let Some(after) = gather.wants() {
            asked += 1;
            gather.add(fake.page(Some(&after)));
        }
        assert_eq!(asked, 2);
        let all: Vec<String> = fake.entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names(&gather.listing()), all);

        let mut shrunk = Gather::new(Fake::new(0, 2500).page(None));
        assert!(shrunk.wants().is_some());
        shrunk.add(Listing::Listed { dir: String::new(), entries: Vec::new(), total: 2500 });
        assert_eq!(shrunk.wants(), None, "an empty page ends it");

        let mut gone = Gather::new(Fake::new(0, 2500).page(None));
        gone.add(Listing::NotFolder);
        assert_eq!(gone.wants(), None);
        assert_eq!(gone.listing(), Listing::NotFolder);
    }
}

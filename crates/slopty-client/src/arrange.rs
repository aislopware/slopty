//! Tidying the canvas: one block of items per repository, newest work on the left.
//!
//! Pure geometry — no document, no camera, no clock — so the interesting parts (determinism,
//! no overlaps, sizes preserved, the order blocks come out in) are ordinary unit tests.
//!
//! The repo key is derived from the terminal's working directory, which the host already
//! sends (`SessionSummary.cwd`, from OSC 7). Nothing here touches a filesystem: two shells
//! belong to the same repo when one's directory contains the other's, which is right for the
//! common case (a shell at the root and shells in its subdirectories) and wrong for a set of
//! shells that are all in sibling subdirectories of the same repo with none at the root. The
//! exact answer is the git root, which only the host can resolve; see DECISIONS.

use slopty_core::ItemId;
use slopty_proto::canvas::Rect;

use crate::canvas::{GAP, snap};

/// Gap between two repository blocks: wider than the gap inside one, so the blocks read as
/// blocks.
pub const BLOCK_GAP: f32 = GAP * 3.0;

/// Height reserved above a block for its heading.
pub const HEADING_HEIGHT: f32 = 28.0;

/// An item to place.
#[derive(Clone, PartialEq, Debug)]
pub struct Arrangeable {
    /// Identity.
    pub id: ItemId,
    /// Current geometry; only the size is kept.
    pub rect: Rect,
    /// Working directory of the item's session, if it has one.
    pub cwd: Option<String>,
    /// Higher is more recent. Orders the blocks, and the items inside a block.
    pub activity: u64,
}

/// A block's heading: where to draw it and what it says.
#[derive(Clone, PartialEq, Debug)]
pub struct Heading {
    /// The repository key this block collects.
    pub key: String,
    /// What to show: the last path component, which is the repository's name.
    pub label: String,
    /// The heading's own rect, directly above the block.
    pub rect: Rect,
    /// The items in the block, so a caller can drop a heading whose items are all gone.
    pub items: Vec<ItemId>,
}

/// What [`arrange_by_repo`] decided.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Arrangement {
    /// Where each item goes, in the order they were given.
    pub places: Vec<(ItemId, Rect)>,
    /// One heading per block, left to right.
    pub headings: Vec<Heading>,
}

impl Arrangement {
    /// The new rect of `id`, if it was placed.
    #[must_use]
    pub fn place(&self, id: ItemId) -> Option<Rect> {
        self.places.iter().find(|(other, _)| *other == id).map(|(_, rect)| *rect)
    }

    /// Every rect the arrangement occupies, headings included: what the camera should fit.
    pub fn rects(&self) -> impl Iterator<Item = Rect> + '_ {
        self.places.iter().map(|(_, r)| *r).chain(self.headings.iter().map(|h| h.rect))
    }
}

/// The directory `path` sits in, normalised: no trailing separator, `~` left alone.
fn clean(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() { "/" } else { trimmed }
}

/// Whether `root` contains `path` (or is it).
fn contains(root: &str, path: &str) -> bool {
    path == root
        || (path.len() > root.len()
            && path.starts_with(root)
            && (root.ends_with('/') || path.as_bytes().get(root.len()) == Some(&b'/')))
}

/// The key two items must share to land in the same block: the shallowest working directory
/// among them that contains this one.
///
/// Items without a directory share the "no repository" key, an empty string, which sorts last.
fn repo_key(cwd: Option<&str>, all: &[&str]) -> String {
    let Some(path) = cwd.map(clean).filter(|p| !p.is_empty()) else {
        return String::new();
    };
    all.iter()
        .filter(|root| contains(root, path))
        .min_by_key(|root| (root.len(), *root))
        .map_or_else(|| path.to_owned(), |root| (*root).to_owned())
}

/// The label a block shows: the last component of its key.
#[must_use]
pub fn label_for(key: &str) -> String {
    if key.is_empty() {
        return "no repository".to_owned();
    }
    key.rsplit('/').find(|part| !part.is_empty()).unwrap_or(key).to_owned()
}

/// Lay the items out in one block per repository.
///
/// Blocks run left to right, most recently active first; inside a block the items keep their
/// sizes and fill a square-ish grid, most recently active first. Ties break on the item id, so
/// the same input always gives the same canvas. `origin` is the top-left the whole arrangement
/// starts at.
#[must_use]
pub fn arrange_by_repo(items: &[Arrangeable], origin: (f32, f32)) -> Arrangement {
    let cleaned: Vec<&str> = items
        .iter()
        .filter_map(|i| i.cwd.as_deref().map(clean))
        .filter(|p| !p.is_empty())
        .collect();
    let mut keyed: Vec<(String, &Arrangeable)> =
        items.iter().map(|item| (repo_key(item.cwd.as_deref(), &cleaned), item)).collect();
    // Blocks by most recent activity, then by key so a tie is still deterministic.
    keyed.sort_by(|(ak, a), (bk, b)| {
        b.activity.cmp(&a.activity).then_with(|| ak.cmp(bk)).then_with(|| a.id.cmp(&b.id))
    });

    let mut order: Vec<String> = Vec::new();
    for (key, _) in &keyed {
        if !order.contains(key) {
            order.push(key.clone());
        }
    }

    let mut out = Arrangement::default();
    let mut x = snap(origin.0);
    for key in &order {
        let block: Vec<&Arrangeable> =
            keyed.iter().filter(|(k, _)| k == key).map(|(_, item)| *item).collect();
        let columns = columns_for(block.len());
        let top = snap(origin.1) + HEADING_HEIGHT;
        let width = place_block(&block, x, top, columns, &mut out.places);
        out.headings.push(Heading {
            key: key.clone(),
            label: label_for(key),
            rect: Rect { x, y: snap(origin.1), w: width.max(1.0), h: HEADING_HEIGHT },
            items: block.iter().map(|item| item.id).collect(),
        });
        x = snap(x + width + BLOCK_GAP);
    }
    // Back into the order they were given, so a caller can zip with its own list.
    out.places.sort_by_key(|(id, _)| items.iter().position(|i| i.id == *id).unwrap_or(usize::MAX));
    out
}

/// A square-ish grid: the number of columns for `n` items.
fn columns_for(n: usize) -> usize {
    let mut columns = 1_usize;
    while columns.saturating_mul(columns) < n {
        columns = columns.saturating_add(1);
    }
    columns.max(1)
}

/// Place one block's items, row by row, and return the block's width.
fn place_block(
    block: &[&Arrangeable],
    left: f32,
    top: f32,
    columns: usize,
    out: &mut Vec<(ItemId, Rect)>,
) -> f32 {
    let mut y = top;
    let mut width = 0.0_f32;
    for row in block.chunks(columns) {
        let mut x = left;
        let mut tallest = 0.0_f32;
        for item in row {
            let rect = Rect { x, y, w: item.rect.w, h: item.rect.h };
            out.push((item.id, rect));
            x = snap(x + item.rect.w + GAP);
            tallest = tallest.max(item.rect.h);
        }
        width = width.max(x - left - GAP);
        y = snap(y + tallest + GAP);
    }
    width.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(cwd: Option<&str>, activity: u64, size: (f32, f32)) -> Arrangeable {
        Arrangeable {
            id: ItemId::new(),
            rect: Rect { x: 999.0, y: -999.0, w: size.0, h: size.1 },
            cwd: cwd.map(str::to_owned),
            activity,
        }
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
    }

    /// Shells in a repository and in its subdirectories are one block; another checkout is its
    /// own; a shell with no directory at all lands in a block of its own at the end.
    #[test]
    fn a_repository_and_its_subdirectories_are_one_block() {
        let items = vec![
            item(Some("/w/slopty"), 30, (200.0, 100.0)),
            item(Some("/w/slopty/crates/ui"), 20, (200.0, 100.0)),
            item(Some("/w/other"), 10, (200.0, 100.0)),
            item(None, 5, (200.0, 100.0)),
        ];
        let out = arrange_by_repo(&items, (0.0, 0.0));
        assert_eq!(out.places.len(), 4);
        assert_eq!(
            out.headings.iter().map(|h| h.label.as_str()).collect::<Vec<_>>(),
            ["slopty", "other", "no repository"],
            "blocks run left to right by most recent activity"
        );
        // The two slopty shells share a block: same heading span, side by side.
        let (a, b) = (out.place(items[0].id).unwrap(), out.place(items[1].id).unwrap());
        assert!((a.y - b.y).abs() < f32::EPSILON, "{a:?} {b:?}");
        assert!(a.x < b.x, "most recent first: {a:?} {b:?}");
        let heading = &out.headings[0];
        assert!(heading.rect.y < a.y, "the heading is above its block");
        assert!(a.x >= heading.rect.x && b.x >= heading.rect.x);
        // The other checkout starts to the right of everything in the first block.
        let other = out.place(items[2].id).unwrap();
        assert!(other.x > b.x + b.w, "{other:?} after {b:?}");
    }

    /// Nothing overlaps, every item keeps its size, and the same input gives the same canvas.
    #[test]
    fn the_layout_is_deterministic_and_never_overlaps() {
        let repos = ["/w/a", "/w/b", "/w/b/deep", "/w/c"];
        let items: Vec<Arrangeable> = (0_u8..11)
            .map(|i| {
                let size = (160.0 + f32::from(i % 3) * 40.0, 90.0 + f32::from(i % 2) * 60.0);
                item(Some(repos[usize::from(i) % repos.len()]), u64::from(20 - i), size)
            })
            .collect();

        let first = arrange_by_repo(&items, (0.0, 0.0));
        let again = arrange_by_repo(&items, (0.0, 0.0));
        assert_eq!(first, again, "same input, same layout");

        for (index, (id, rect)) in first.places.iter().enumerate() {
            let source = items.iter().find(|i| i.id == *id).expect("placed what we gave it");
            assert!(
                (rect.w - source.rect.w).abs() < f32::EPSILON
                    && (rect.h - source.rect.h).abs() < f32::EPSILON,
                "size preserved: {rect:?} vs {:?}",
                source.rect
            );
            assert_eq!(*id, items[index].id, "places come back in the order given");
        }
        for (i, (_, a)) in first.places.iter().enumerate() {
            for (_, b) in first.places.iter().skip(i + 1) {
                assert!(!overlaps(*a, *b), "{a:?} overlaps {b:?}");
            }
            for heading in &first.headings {
                assert!(!overlaps(*a, heading.rect), "{a:?} overlaps heading {:?}", heading.rect);
            }
        }
    }

    /// The block order follows activity, not the order the items arrived in.
    #[test]
    fn blocks_are_ordered_by_the_most_recent_work_in_them() {
        let items = vec![
            item(Some("/w/quiet"), 1, (100.0, 100.0)),
            item(Some("/w/busy"), 99, (100.0, 100.0)),
            item(Some("/w/quiet"), 2, (100.0, 100.0)),
        ];
        let out = arrange_by_repo(&items, (0.0, 0.0));
        assert_eq!(
            out.headings.iter().map(|h| h.label.as_str()).collect::<Vec<_>>(),
            ["busy", "quiet"]
        );
        assert!(out.place(items[1].id).unwrap().x < out.place(items[0].id).unwrap().x);
    }

    /// Nothing to arrange is not a crash, and one item is its own block.
    #[test]
    fn an_empty_canvas_and_a_single_item() {
        assert_eq!(arrange_by_repo(&[], (0.0, 0.0)), Arrangement::default());
        let only = vec![item(Some("/w/solo/"), 1, (300.0, 200.0))];
        let out = arrange_by_repo(&only, (32.0, 48.0));
        assert_eq!(out.headings.len(), 1);
        assert_eq!(out.headings[0].label, "solo", "a trailing separator is not a component");
        let rect = out.place(only[0].id).unwrap();
        assert!(rect.x >= 32.0 && rect.y >= 48.0, "{rect:?} starts at the origin given");
    }

    /// Paths that merely share a prefix of characters are different repositories.
    #[test]
    fn a_shared_prefix_is_not_a_shared_repository() {
        let items = vec![
            item(Some("/w/slopty"), 2, (100.0, 100.0)),
            item(Some("/w/slopty-wt"), 1, (100.0, 100.0)),
        ];
        let out = arrange_by_repo(&items, (0.0, 0.0));
        assert_eq!(out.headings.len(), 2, "{:?}", out.headings);
    }
}

//! The files an `@` mention in the conversation face's composer picks from: every path under
//! the agent's working directory, walked once and matched on each keystroke.
//!
//! The walk skips hidden entries and what `.gitignore` files exclude, git repository or not,
//! as the palette's quick open does (`crate::find`), and stops at [`MAX_PATHS`] so a home
//! directory never walks forever. The caller keeps an [`Index`] per directory and builds it
//! again when the agent may have changed the tree (the follow task's hook count moved).

use std::path::Path;

/// How deep the walk goes: enough for any source tree, bounded for a home directory.
const MAX_DEPTH: usize = 12;

/// How many paths the walk keeps before it stops.
pub const MAX_PATHS: usize = 50_000;

/// The paths under one directory, ready to match.
#[derive(Debug, Default)]
pub struct Index {
    /// Relative to the root, a directory ending in `/`, in walk order.
    paths: Vec<String>,
    /// The same paths lowercased, for matching.
    lower: Vec<String>,
}

impl Index {
    /// Walk `root`.
    #[must_use]
    pub fn build(root: &Path) -> Self {
        let walk = ignore::WalkBuilder::new(root)
            .max_depth(Some(MAX_DEPTH))
            .require_git(false)
            .sort_by_file_name(Ord::cmp)
            .build();
        let mut paths = Vec::new();
        for entry in walk.flatten() {
            let Ok(relative) = entry.path().strip_prefix(root) else { continue };
            if relative.as_os_str().is_empty() {
                continue;
            }
            let mut path = relative.to_string_lossy().into_owned();
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                path.push('/');
            }
            paths.push(path);
            if paths.len() >= MAX_PATHS {
                break;
            }
        }
        let lower = paths.iter().map(|p| p.to_lowercase()).collect();
        Self { paths, lower }
    }

    /// Paths the walk kept.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the walk found nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The paths `query` matches, best first, at most `limit`; with an empty query, the
    /// directory's own entries.
    ///
    /// Case never matters. A path ranks by how its name meets the query: its last component
    /// starting with it, then the whole path starting with it, then its name holding it, then
    /// the path holding it, then the query's characters in order anywhere in the path. Within a
    /// rank, shorter paths come first.
    #[must_use]
    pub fn matching(&self, query: &str, limit: usize) -> Vec<String> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return self
                .paths
                .iter()
                .filter(|p| !p.trim_end_matches('/').contains('/'))
                .take(limit)
                .cloned()
                .collect();
        }
        let mut ranked: Vec<(u8, usize, usize)> = self
            .lower
            .iter()
            .enumerate()
            .filter_map(|(ix, path)| Some((rank(path, &needle)?, path.len(), ix)))
            .collect();
        ranked.sort_unstable();
        ranked
            .into_iter()
            .take(limit)
            .filter_map(|(_, _, ix)| self.paths.get(ix).cloned())
            .collect()
    }
}

/// How `path` meets `needle`, both lowercase; lower is better, `None` for no match.
fn rank(path: &str, needle: &str) -> Option<u8> {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path);
    if name.starts_with(needle) {
        Some(0)
    } else if path.starts_with(needle) {
        Some(1)
    } else if name.contains(needle) {
        Some(2)
    } else if path.contains(needle) {
        Some(3)
    } else {
        let mut chars = path.chars();
        needle.chars().all(|c| chars.any(|p| p == c)).then_some(4)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A mention finds names before paths and paths before scattered letters, never what git
    /// ignores or what is hidden, and an empty query lists the top of the tree.
    #[test]
    fn a_mention_ranks_names_first_and_skips_the_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for d in ["src/view", "target/debug", ".git", "docs"] {
            fs::create_dir_all(root.join(d)).expect("dirs");
        }
        for f in [
            "src/main.rs",
            "src/view/mention.rs",
            "src/view/menu.rs",
            "docs/manual.md",
            "target/debug/main",
            ".git/HEAD",
            ".env",
            "README.md",
        ] {
            fs::write(root.join(f), "").expect("file");
        }
        fs::write(root.join(".gitignore"), "target\n").expect("ignore");
        let index = Index::build(root);

        assert_eq!(index.matching("men", 8), ["src/view/menu.rs", "src/view/mention.rs"]);
        assert_eq!(index.matching("ma", 8), ["src/main.rs", "docs/manual.md"]);
        assert_eq!(index.matching("src/v", 2), ["src/view/", "src/view/menu.rs"], "capped");
        assert_eq!(index.matching("svm", 8), ["src/view/menu.rs", "src/view/mention.rs"]);
        assert_eq!(index.matching("MAIN", 8), ["src/main.rs"], "case does not matter");
        assert!(index.matching("head", 8).is_empty(), "hidden");
        assert!(index.matching("debug", 8).is_empty(), "ignored");
        assert_eq!(index.matching("", 8), ["README.md", "docs/", "src/"]);
    }
}

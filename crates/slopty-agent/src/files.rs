//! Paths under an agent's working directory for the composer's `@file` completion.

use std::path::Path;

/// How deep the walk goes: enough for any source tree, bounded for a home directory.
const MAX_DEPTH: usize = 8;
/// How many entries the walk visits before it stops: a home directory can be endless.
const MAX_VISITED: usize = 20_000;

/// The paths under `root` that `query` matches, best first, at most `limit`.
///
/// A path whose last component starts with the query ranks first, then any path containing
/// it, shorter first, all case-insensitive. Relative to `root`, a directory ending in `/`.
/// Hidden entries and what `.gitignore` files exclude are skipped, git repository or not.
/// Nothing for an empty query.
#[must_use]
pub fn matching(root: &Path, query: &str, limit: usize) -> Vec<String> {
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }
    let needle = query.to_ascii_lowercase();
    let mut found: Vec<(u8, usize, String)> = Vec::new();
    let walk = ignore::WalkBuilder::new(root)
        .max_depth(Some(MAX_DEPTH))
        .require_git(false)
        .sort_by_file_name(Ord::cmp)
        .build();
    for entry in walk.take(MAX_VISITED).flatten() {
        let Ok(relative) = entry.path().strip_prefix(root) else { continue };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        let mut path = relative.to_string_lossy().into_owned();
        if is_dir {
            path.push('/');
        }
        let lower = path.to_ascii_lowercase();
        if !lower.contains(&needle) {
            continue;
        }
        let name = lower.trim_end_matches('/').rsplit('/').next().unwrap_or(&lower);
        let rank = u8::from(!name.starts_with(&needle));
        found.push((rank, path.len(), path));
    }
    found.sort();
    found.into_iter().take(limit).map(|(_rank, _len, path)| path).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn a_name_that_starts_with_the_query_ranks_first_and_ignored_paths_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src/manual")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("src/manual/index.md"), "").unwrap();
        fs::write(root.join("target/main.o"), "").unwrap();
        fs::write(root.join(".git/HEAD"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();
        fs::write(root.join(".gitignore"), "target\n").unwrap();

        assert_eq!(matching(root, "ma", 8), ["src/main.rs", "src/manual/", "src/manual/index.md"]);
        assert_eq!(matching(root, "MAIN", 8), ["src/main.rs"], "case does not matter");
        assert_eq!(matching(root, "readme", 8), ["README.md"]);
        assert_eq!(matching(root, "src", 1), ["src/"], "capped");
        assert!(matching(root, "head", 8).is_empty(), "hidden");
        assert!(matching(root, "", 8).is_empty());
    }
}

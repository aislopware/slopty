//! Quick open: the paths under a directory that a typed query matches, fuzzily, best first.
//!
//! Inside a git worktree the paths come from an index of the whole worktree ([`Index`]): one
//! parallel walk that honours `.gitignore`, `.ignore` and the repository's excludes, kept fresh
//! by the file system's own events (`watch`: one `FSEvents` stream over the tree on macOS, an
//! inotify watch on each indexed directory on Linux) or, where there are none or too few, by
//! looking at the indexed directories' modification times before a query. A keystroke then costs a
//! match over memory, not a walk of the disk. Outside a worktree (a home directory, `/`) a walk
//! bounded in depth and size answers each query, ranked the same way.
//!
//! The match is `nucleo-matcher`'s (Helix's port of fzf's scoring) in its path mode, smart case:
//! each word typed must match, its letters in order, with bonuses where one starts a name after
//! a `/` or a word inside one. Ties go to the shorter path.

mod index;
mod watch;

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};

pub use index::{Answer, Index, Indexes};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// How deep the walk outside a worktree goes: enough for any source tree, bounded for a home
/// directory.
const MAX_DEPTH: usize = 8;
/// How many entries the walk outside a worktree visits before it stops: a home directory can be
/// endless.
const MAX_VISITED: usize = 20_000;

/// The paths under `root` that `query` matches, best first, at most `limit`.
///
/// Relative to `root`, a directory ending in `/`. Hidden entries and what the ignore files
/// exclude are skipped, git repository or not. Nothing for an empty query.
///
/// Answered from the worker's shared index of the worktree `root` is in ([`Indexes::shared`]),
/// built on the first query, with what the person should know of it the first time there is
/// something; outside a worktree, from a bounded walk.
#[must_use]
pub fn matching(root: &Path, query: &str, limit: usize) -> Answer {
    Indexes::shared().matching(root, query, limit)
}

/// The worktree `dir` is in: the nearest directory up from it that holds `.git` (a repository's
/// directory, or a linked worktree's file).
#[must_use]
pub fn worktree_of(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|at| at.join(".git").exists()).map(Path::to_path_buf)
}

/// A walk of `root` as quick open sees a tree: ignore files honoured whether or not it is a
/// repository, hidden entries skipped, symbolic links not followed.
fn walker(root: &Path, depth: Option<usize>) -> ignore::WalkBuilder {
    let mut walk = ignore::WalkBuilder::new(root);
    walk.max_depth(depth).require_git(false).follow_links(false);
    walk
}

/// The paths under `root` that `query` matches by a walk bounded in depth and entries: for a
/// directory no worktree holds.
fn walk_matching(root: &Path, query: &str, limit: usize) -> Vec<String> {
    let mut found = Vec::new();
    let walk = walker(root, Some(MAX_DEPTH)).sort_by_file_name(Ord::cmp).build();
    for entry in walk.take(MAX_VISITED).flatten() {
        let Ok(relative) = entry.path().strip_prefix(root) else { continue };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let mut path = relative.to_string_lossy().into_owned();
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            path.push('/');
        }
        found.push(path);
    }
    let all: Vec<usize> = (0..found.len()).collect();
    let best = rank(&found, 0, &all, query, limit).best;
    best.into_iter().filter_map(|at| found.get(at).cloned()).collect()
}

/// Candidates each thread scores at least: below twice this a query runs on the calling thread,
/// since a thread costs more than scoring a few thousand paths.
const PER_THREAD: usize = 16_384;

/// Characters that give a word of the query a meaning of its own (`!` not, `^` starts, `$`
/// ends, `'` exact, `\` escapes): typed after a query, they need not narrow it.
const SPECIAL: [char; 5] = ['!', '^', '$', '\'', '\\'];

/// Whether every path `later` matches is one `earlier` matched, so the matches of `earlier`
/// are all `later` needs to score: a query typed on with plain characters. A fuzzy word's
/// letters in order are a subsequence of the longer word's, and a new word only adds a
/// condition.
fn narrows(earlier: &str, later: &str) -> bool {
    later.starts_with(earlier) && !later.contains(SPECIAL)
}

/// A query's matches among some candidates, as indices into the paths.
#[derive(Debug, Default)]
pub(crate) struct Ranking {
    /// Every candidate that matched, in the candidates' order.
    matched: Vec<usize>,
    /// The best at most `limit` of them, best first.
    best: Vec<usize>,
}

/// A path's place in the ranking: its score, then the shorter, then by name, then where it is.
type Key<'a> = (u32, Reverse<usize>, Reverse<&'a str>, usize);

/// Rank the `candidates` (indices into `paths`, each seen without its first `skip` bytes) for
/// `query`: highest score first, then the shorter path, then by name. Nothing for a query with
/// no word in it. A long list is split across the cores.
pub(crate) fn rank<S: AsRef<str> + Sync>(
    paths: &[S],
    skip: usize,
    candidates: &[usize],
    query: &str,
    limit: usize,
) -> Ranking {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    if pattern.atoms.is_empty() || limit == 0 {
        return Ranking::default();
    }
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let threads = (candidates.len() / PER_THREAD).clamp(1, cores);
    let parts: Vec<(Vec<usize>, Vec<Key<'_>>)> = if threads == 1 {
        vec![score(paths, skip, candidates, &pattern, limit)]
    } else {
        let chunk = candidates.len().div_ceil(threads);
        std::thread::scope(|scope| {
            let pattern = &pattern;
            #[expect(clippy::needless_collect, reason = "every thread starts before one is joined")]
            let running: Vec<_> = candidates
                .chunks(chunk)
                .map(|part| scope.spawn(move || score(paths, skip, part, pattern, limit)))
                .collect();
            running
                .into_iter()
                .map(|thread| {
                    thread.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect()
        })
    };
    let mut ranking = Ranking::default();
    let mut best = Vec::new();
    for (matched, kept) in parts {
        ranking.matched.extend(matched);
        best.extend(kept);
    }
    best.sort_unstable_by(|a, b| b.cmp(a));
    ranking.best = best.into_iter().take(limit).map(|(.., at)| at).collect();
    ranking
}

/// One thread's share of [`rank`]: the candidates that match, and the best `limit` of them.
fn score<'a, S: AsRef<str>>(
    paths: &'a [S],
    skip: usize,
    candidates: &[usize],
    pattern: &Pattern,
    limit: usize,
) -> (Vec<usize>, Vec<Key<'a>>) {
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut chars = Vec::new();
    let mut matched = Vec::new();
    // The worst kept on top, so a better one replaces it.
    let mut best: BinaryHeap<Reverse<Key<'a>>> = BinaryHeap::with_capacity(limit.saturating_add(1));
    for &at in candidates {
        let Some(path) = paths.get(at).and_then(|p| p.as_ref().get(skip..)) else { continue };
        let Some(score) = pattern.score(Utf32Str::new(path, &mut chars), &mut matcher) else {
            continue;
        };
        matched.push(at);
        best.push(Reverse((score, Reverse(path.len()), Reverse(path), at)));
        if best.len() > limit {
            best.pop();
        }
    }
    (matched, best.into_iter().map(|Reverse(key)| key).collect())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// [`rank`] over all of `paths`, as the paths it keeps.
    fn ranked<'a>(paths: &[&'a str], query: &str, limit: usize) -> Vec<&'a str> {
        let all: Vec<usize> = (0..paths.len()).collect();
        let best = rank(paths, 0, &all, query, limit).best;
        best.into_iter().map(|at| paths[at]).collect()
    }

    #[test]
    fn the_best_match_leads_and_a_name_beats_a_scatter() {
        let paths = [
            "docs/manual/index.md",
            "src/main.rs",
            "src/maintenance/",
            "crates/slopty-ui/src/terminal/view.rs",
            "README.md",
        ];
        assert_eq!(
            ranked(&paths, "main", 3),
            ["src/main.rs", "src/maintenance/", "docs/manual/index.md"]
        );
        assert_eq!(ranked(&paths, "termview", 1), ["crates/slopty-ui/src/terminal/view.rs"]);
        assert_eq!(ranked(&paths, "src view", 5), ["crates/slopty-ui/src/terminal/view.rs"]);
        assert_eq!(ranked(&paths, "MAIN", 5), Vec::<&str>::new(), "a capital is exact");
        assert!(ranked(&paths, "  ", 5).is_empty());
        assert!(ranked(&paths, "main", 0).is_empty());
    }

    #[test]
    fn typing_on_narrows_and_a_special_character_or_a_deletion_does_not() {
        assert!(narrows("vi", "view"));
        assert!(narrows("view", "view src"), "a new word only adds a condition");
        assert!(narrows("view", "viewS"), "a capital is stricter");
        assert!(!narrows("view", "vi"), "a deletion widens");
        assert!(!narrows("view", "view !test"), "a negation widens");
        assert!(!narrows("src", "src$"));
        assert!(!narrows("src", "rsc"));
    }

    #[test]
    fn split_across_threads_the_ranking_is_the_one_thread_ranking() {
        let paths: Vec<String> =
            (0..PER_THREAD * 5).map(|n| format!("crate{}/src/item_{n}_view.rs", n % 97)).collect();
        let all: Vec<usize> = (0..paths.len()).collect();
        let pattern = Pattern::parse("e9 view", CaseMatching::Smart, Normalization::Smart);
        let (matched, mut best) = score(&paths, 0, &all, &pattern, 20);
        best.sort_unstable_by(|a, b| b.cmp(a));
        let alone: Vec<usize> = best.into_iter().map(|(.., at)| at).collect();
        let split = rank(&paths, 0, &all, "e9 view", 20);
        assert_eq!(split.best, alone);
        assert_eq!(split.matched, matched, "in the candidates' order");
        assert!(split.matched.len() < paths.len() && split.best.len() == 20);
    }

    #[test]
    fn outside_a_worktree_a_bounded_walk_answers_and_ignored_paths_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src/manual")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("src/manual/index.md"), "").unwrap();
        fs::write(root.join("target/main.o"), "").unwrap();
        fs::write(root.join(".hidden/main.txt"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();
        fs::write(root.join(".gitignore"), "target\n").unwrap();
        assert_eq!(worktree_of(root), None, "no repository here");

        assert_eq!(walk_matching(root, "main", 8), ["src/main.rs", "src/manual/index.md"]);
        assert_eq!(walk_matching(root, "readme", 8), ["README.md"]);
        assert_eq!(walk_matching(root, "man", 1), ["src/manual/"], "capped");
        assert!(walk_matching(root, "", 8).is_empty());
    }
}

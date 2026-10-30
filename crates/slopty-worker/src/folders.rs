//! New project's folders on this machine: the ones the person goes to most, and any a query
//! matches, however deep, under the home or on a drive they work on.
//!
//! The ones gone to most are their directory jumper's (zoxide), as it ranks them. The others
//! come from an index of every folder under the home and under each drive a folder gone to is
//! on (`/Volumes/Work`), walked in parallel as `fd` walks (the `ignore` crate): hidden folders,
//! what the ignore files exclude, `~/Library` and what builds and package managers make left
//! out. An index is built on the first ask and walked again in the background once it is two
//! minutes old; until a drive's first is built, Spotlight's index answers for the home
//! by name (on a Mac), and the answer says the index is still being made.
//!
//! A query ranks them as quick open ranks paths: fzf's scoring (`nucleo-matcher`, Helix's port)
//! in its path mode, smart case, over each folder's path (under the home without the home's
//! own), so `oss/slo` finds `~/Workspace/oss/slopty`. A folder whose own name holds the query's
//! last word gains, as a project is looked for by its name, and a folder the person goes to
//! gains by how often and how lately, as zoxide counts. With nothing typed, the folders gone to
//! most, most first.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use parking_lot::Mutex;
use slopty_proto::folder::FolderHit;
use tokio::time::Instant;

/// How long zoxide or Spotlight may take to answer.
const ASK_WAIT: Duration = Duration::from_secs(3);

/// The most folders Spotlight's answer is read for: a short name matches many.
const SPOTLIGHT_MAX: usize = 5_000;

/// The most bytes of a program's answer read.
const ANSWER_MAX: usize = 4 << 20;

/// How long a machine with no zoxide is not asked to find one again.
const ZOXIDE_RETRY: Duration = Duration::from_secs(300);

/// How much a folder gone to gains, per unit of the logarithm of its zoxide score, on fzf's
/// score of its path: a folder visited hundreds of times gains about what a word's start does.
const VISITED_GAIN: f64 = 16.0;

/// How much a folder whose own name holds the query's last word gains.
const NAMED_GAIN: f64 = 40.0;

/// How old an index may be before it is walked again, in the background.
const INDEX_FRESH: Duration = Duration::from_secs(120);

/// The most folders one index holds.
const INDEX_MAX: usize = 1_000_000;

/// Folders a project is never in: what a build or a package manager makes.
const NEVER: [&str; 4] = ["node_modules", "target", "build", "dist"];

/// The folders for `query` under `home` and the drives the person works on, best first, at
/// most `limit`, and whether an index is still being made.
pub async fn folders(home: &Path, query: &str, limit: usize) -> (Vec<FolderHit>, bool) {
    let query = query.trim();
    let visited = visited().await;
    let roots = roots(home, &visited);
    let (indexed, indexing) = indexed(&roots);
    let spotlight =
        if indexing && !query.is_empty() { named(home, query).await } else { Vec::new() };
    let (home, query) = (home.to_path_buf(), query.to_owned());
    let ranked = tokio::task::spawn_blocking(move || {
        let found: Vec<&str> = indexed
            .iter()
            .flat_map(|dirs| dirs.iter().map(String::as_str))
            .chain(spotlight.iter().map(String::as_str))
            .collect();
        let mut hits = rank(&home, &query, &visited, &found, limit);
        for hit in &mut hits {
            hit.repo = Path::new(&hit.path).join(".git").exists();
        }
        hits
    });
    (ranked.await.unwrap_or_default(), indexing)
}

/// Where the indexes are walked from: the home, and each drive a folder gone to is on.
fn roots(home: &Path, visited: &[(f64, String)]) -> Vec<PathBuf> {
    let mut roots = vec![home.to_path_buf()];
    for (_, path) in visited {
        let mut parts = Path::new(path).components();
        let drive = match (parts.next(), parts.next(), parts.next()) {
            (Some(root), Some(volumes), Some(name)) if volumes.as_os_str() == "Volumes" => {
                Path::new(root.as_os_str()).join(volumes).join(name)
            }
            _ => continue,
        };
        if !roots.contains(&drive) {
            roots.push(drive);
        }
    }
    roots
}

/// One index: its folders, absolute, when they were walked, and whether a walk is under way.
#[derive(Default)]
struct Index {
    dirs: Option<(Instant, Arc<[String]>)>,
    walking: bool,
}

/// Every index, by where it is walked from.
static INDEXES: LazyLock<Mutex<HashMap<PathBuf, Index>>> = LazyLock::new(Mutex::default);

/// The folders indexed under `roots`, and whether one is still being made; a walk starts for
/// each index not made or no longer fresh.
fn indexed(roots: &[PathBuf]) -> (Vec<Arc<[String]>>, bool) {
    let mut indexes = INDEXES.lock();
    let mut found = Vec::new();
    let mut indexing = false;
    for root in roots {
        let index = indexes.entry(root.clone()).or_default();
        let stale = index.dirs.as_ref().is_none_or(|(at, _)| at.elapsed() >= INDEX_FRESH);
        if stale && !index.walking {
            index.walking = true;
            let root = root.clone();
            std::thread::spawn(move || {
                let dirs: Arc<[String]> = walk(&root).into();
                let walked = Index { dirs: Some((Instant::now(), dirs)), walking: false };
                INDEXES.lock().insert(root, walked);
            });
        }
        match &index.dirs {
            Some((_, dirs)) => found.push(Arc::clone(dirs)),
            None => indexing = true,
        }
    }
    drop(indexes);
    (found, indexing)
}

/// Every folder under `root` a project may be, absolute, walked in parallel: hidden ones, what
/// the ignore files exclude, a home's `Library` and what builds make left out.
#[must_use]
pub fn walk(root: &Path) -> Vec<String> {
    let found = Mutex::new(Vec::new());
    let library = root.join("Library");
    let walk = ignore::WalkBuilder::new(root)
        .hidden(true)
        .require_git(false)
        .follow_links(false)
        .filter_entry(move |entry| {
            let name = entry.file_name().to_string_lossy();
            entry.file_type().is_some_and(|kind| kind.is_dir())
                && !NEVER.contains(&name.as_ref())
                && entry.path() != library
        })
        .build_parallel();
    walk.run(|| {
        Box::new(|entry| {
            let Ok(entry) = entry else { return ignore::WalkState::Continue };
            if entry.depth() == 0 {
                return ignore::WalkState::Continue;
            }
            let mut found = found.lock();
            if found.len() >= INDEX_MAX {
                return ignore::WalkState::Quit;
            }
            found.push(entry.path().to_string_lossy().into_owned());
            ignore::WalkState::Continue
        })
    });
    found.into_inner()
}

/// The folders zoxide knows the person goes to, with its score, the folders still there.
async fn visited() -> Vec<(f64, String)> {
    let Some(zoxide) = zoxide().await else { return Vec::new() };
    let mut ask = tokio::process::Command::new(&zoxide.program);
    ask.args(["query", "--list", "--score"]).env("PATH", &zoxide.path);
    let answer = run(ask).await.unwrap_or_default();
    zoxide_list(&answer).into_iter().filter(|(_, path)| Path::new(path).is_dir()).collect()
}

/// When zoxide was looked for, and what was found.
type Looked = (Instant, Option<crate::facts::Installed>);

/// zoxide as the person's terminal finds it; `None` where there is none, asked again only after
/// [`ZOXIDE_RETRY`].
async fn zoxide() -> Option<crate::facts::Installed> {
    static FOUND: LazyLock<Mutex<Option<Looked>>> = LazyLock::new(Mutex::default);
    let known = FOUND.lock().clone();
    if let Some((at, found)) = known
        && (found.is_some() || at.elapsed() < ZOXIDE_RETRY)
    {
        return found;
    }
    let found = crate::facts::installed("zoxide", None).await;
    *FOUND.lock() = Some((Instant::now(), found.clone()));
    found
}

/// The folders under `home` whose names hold the last word of `query`, as Spotlight's index has
/// them: what answers while the home's own index is made. Nothing off a Mac.
async fn named(home: &Path, query: &str) -> Vec<String> {
    let Some(word) = last_word(query).filter(|_| cfg!(target_os = "macos")) else {
        return Vec::new();
    };
    let mut ask = tokio::process::Command::new("/usr/bin/mdfind");
    ask.arg("-onlyin").arg(home).arg(spotlight_query(word));
    let answer = run(ask).await.unwrap_or_default();
    answer.lines().filter(|line| !line.is_empty()).take(SPOTLIGHT_MAX).map(str::to_owned).collect()
}

/// Run `ask` for its answer, within [`ASK_WAIT`]; `None` when it fails or takes longer.
async fn run(mut ask: tokio::process::Command) -> Option<String> {
    ask.stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(ASK_WAIT, ask.output()).await.ok()?.ok()?;
    let mut bytes = out.stdout;
    bytes.truncate(ANSWER_MAX);
    out.status.success().then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// zoxide's `query --list --score` answer: each folder with its score, as zoxide ranks them.
#[must_use]
pub fn zoxide_list(answer: &str) -> Vec<(f64, String)> {
    answer
        .lines()
        .filter_map(|line| {
            let (score, path) = line.trim_start().split_once(' ')?;
            let (score, path) = (score.parse().ok()?, path.trim_start());
            path.starts_with('/').then(|| (score, path.to_owned()))
        })
        .collect()
}

/// The word of `query` a folder's own name is looked up by: its last, after a space or a `/`.
fn last_word(query: &str) -> Option<&str> {
    query.rsplit([' ', '/']).find(|word| !word.is_empty())
}

/// Spotlight's query for the folders whose names hold `word`, in any case.
#[must_use]
pub fn spotlight_query(word: &str) -> String {
    let mut quoted = String::with_capacity(word.len());
    for c in word.chars() {
        if matches!(c, '"' | '\\' | '*' | '?') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    format!("kMDItemContentType == \"public.folder\" && kMDItemFSName == \"*{quoted}*\"cd")
}

/// `path` as the person reads it under `home` (`~`'s own spelled out), `None` for one a project
/// is never in: hidden, or what a build or a package manager makes.
fn shown(home: &str, path: &str) -> Option<String> {
    let home = home.trim_end_matches('/');
    let under = path.strip_prefix(home).and_then(|rest| rest.strip_prefix('/'));
    let parts = under.unwrap_or(path).trim_matches('/');
    let kept = parts
        .split('/')
        .all(|part| !part.starts_with('.') && !NEVER.contains(&part) && part != "Library");
    (kept && !parts.is_empty()).then(|| parts.to_owned())
}

/// The folders `visited` (with zoxide's scores) and `found` for `query`, best first, at most
/// `limit`.
///
/// With nothing typed, the visited ones, most first. Else each that matches, by fzf's score of
/// its path under `home`, a visited one gaining by how much it is gone to, ties to the shorter.
#[must_use]
pub fn rank(
    home: &Path,
    query: &str,
    visited: &[(f64, String)],
    found: &[&str],
    limit: usize,
) -> Vec<FolderHit> {
    let home = home.to_string_lossy();
    let hit = |path: &str, visited: bool| FolderHit {
        path: path.trim_end_matches('/').to_owned(),
        repo: false,
        visited,
    };
    if query.is_empty() {
        let mut most: Vec<&(f64, String)> =
            visited.iter().filter(|(_, path)| shown(&home, path).is_some()).collect();
        most.sort_by(|a, b| b.0.total_cmp(&a.0));
        return most.into_iter().take(limit).map(|(_, path)| hit(path, true)).collect();
    }
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let word = last_word(query).map(str::to_lowercase);
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut chars = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let candidates = visited
        .iter()
        .map(|(score, path)| (Some(*score), path.as_str()))
        .chain(found.iter().map(|path| (None, *path)));
    let mut scored: Vec<(f64, usize, FolderHit)> = Vec::new();
    for (zoxide, path) in candidates {
        let path = path.trim_end_matches('/');
        if !seen.insert(path) {
            continue;
        }
        let Some(read) = shown(&home, path) else { continue };
        let Some(score) = pattern.score(Utf32Str::new(&read, &mut chars), &mut matcher) else {
            continue;
        };
        let visited = zoxide.map_or(0.0, |z| z.max(0.0).ln_1p() * VISITED_GAIN);
        let own = read.rsplit('/').next().unwrap_or_default().to_lowercase();
        let named = if word.as_ref().is_some_and(|word| own.contains(word.as_str())) {
            NAMED_GAIN
        } else {
            0.0
        };
        let gain = visited + named;
        scored.push((f64::from(score) + gain, read.len(), hit(path, zoxide.is_some())));
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|(.., hit)| hit).collect()
}

/// Where the person's home is.
#[must_use]
pub fn home() -> PathBuf {
    crate::file::expand_home(Path::new("~"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoxides_list_is_read_with_its_scores() {
        let answer = "  24.0 /Users/dev/oss/slopty\n   4.5 /Users/dev/notes\nnoise\n";
        assert_eq!(
            zoxide_list(answer),
            [(24.0, "/Users/dev/oss/slopty".to_owned()), (4.5, "/Users/dev/notes".to_owned())]
        );
    }

    #[test]
    fn spotlight_is_asked_for_folders_by_a_name_it_cannot_misread() {
        assert_eq!(
            spotlight_query("a\"b*"),
            "kMDItemContentType == \"public.folder\" && kMDItemFSName == \"*a\\\"b\\**\"cd"
        );
        assert_eq!(last_word("oss/slo"), Some("slo"));
        assert_eq!(last_word("my proj "), Some("proj"));
    }

    #[test]
    fn a_query_ranks_by_the_path_and_a_folder_gone_to_gains() {
        let home = Path::new("/Users/dev");
        let visited = [(300.0, "/Users/dev/work/slopty-old".to_owned())];
        let found = [
            "/Users/dev/oss/slopty",
            "/Users/dev/work/slopty-old",
            "/Users/dev/.cache/slopty",
            "/Users/dev/oss/slopty/node_modules/slopty",
            "/Users/dev/notes",
            "/Users/dev/oss/slopty/crates/ui",
        ];
        let paths = |hits: Vec<FolderHit>| hits.into_iter().map(|h| h.path).collect::<Vec<_>>();
        assert_eq!(
            paths(rank(home, "slopty", &visited, &found, 10)),
            [
                "/Users/dev/work/slopty-old",
                "/Users/dev/oss/slopty",
                "/Users/dev/oss/slopty/crates/ui"
            ],
            "the one gone to first, then the one named so; hidden and built ones left out, once"
        );
        assert_eq!(
            paths(rank(home, "oss/slo", &[], &found, 10)),
            ["/Users/dev/oss/slopty", "/Users/dev/oss/slopty/crates/ui"]
        );
        let most =
            rank(home, "", &[(1.0, "/Users/dev/a".into()), (9.0, "/Users/dev/b".into())], &[], 10);
        assert_eq!(paths(most), ["/Users/dev/b", "/Users/dev/a"], "nothing typed: gone to most");
    }

    #[test]
    fn the_drives_a_folder_gone_to_is_on_are_indexed_beside_the_home() {
        let visited = [
            (9.0, "/Volumes/Work/oss/slopty".to_owned()),
            (3.0, "/Volumes/Work/notes".to_owned()),
            (2.0, "/opt/tools".to_owned()),
        ];
        assert_eq!(
            roots(Path::new("/Users/dev"), &visited),
            [PathBuf::from("/Users/dev"), PathBuf::from("/Volumes/Work")]
        );
    }

    #[test]
    fn an_index_holds_every_folder_but_hidden_ignored_and_built_ones() {
        let root = tempfile::tempdir().expect("a folder");
        for dir in ["oss/slopty/src", "oss/slopty/node_modules/x", ".cache/y", "Library/z", "gen"] {
            std::fs::create_dir_all(root.path().join(dir)).expect("made");
        }
        std::fs::write(root.path().join(".gitignore"), "gen/\n").expect("written");
        let base = root.path().to_string_lossy().into_owned();
        let mut found: Vec<String> = walk(root.path())
            .into_iter()
            .map(|path| path.strip_prefix(&base).unwrap_or(&path).to_owned())
            .collect();
        found.sort();
        assert_eq!(found, ["/oss", "/oss/slopty", "/oss/slopty/src"]);
    }
}

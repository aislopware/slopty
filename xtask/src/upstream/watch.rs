//! `xtask upstream watch`: one line per change upstream, as it happens.
//!
//! Every `--interval` it asks each watched upstream on GitHub for its default branch's head
//! (`git ls-remote`) and its most recently updated pull requests (`gh pr list`), and prints a
//! line for what changed since it last looked: the head moved (with its subject and how many
//! commits it is past the base in `xtask/upstream.toml`), or a pull request was opened, updated,
//! merged, closed or reopened. It never syncs: the session that reads the lines decides.
//!
//! The forks' upstreams are watched, and the vendored noq. zed is left out: it merges
//! hundreds of pull requests a week, and `upstream check` reads what an import would take from it.
//! An upstream with `paths` in the config (ghostty, which lands many changes a day, most of them
//! in parts libghostty-vt never builds) is filtered: a head move is named only when the commits
//! since the last look change a file under those paths, and a pull request only when it does.
//! What it saw is kept in `target/upstream-watch/state.json`, so a restart does not announce it
//! again; the first round only records. An upstream that cannot be reached is skipped for the
//! round.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::time::Duration;

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use xshell::{Shell, cmd};

use super::{COMPARE_FILES, Config, compare, relevant, repo_slug, short, touching};

/// Pull requests asked for per upstream: the most recently updated.
const PULLS: &str = "20";
/// `gh pr list` lists at most this many files of a pull request.
const PULL_FILES: usize = 100;

/// One watched upstream.
struct Watched<'a> {
    /// `owner/repo`.
    slug: String,
    url: &'a str,
    branch: &'a str,
    /// The upstream commit last taken.
    base: &'a str,
    /// Only changes under these count (none: every change does).
    paths: &'a [String],
}

/// What one upstream looked like.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The default branch's head.
    pub head: Option<String>,
    /// Pull requests by number.
    pub pulls: BTreeMap<u64, Pull>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pull {
    /// `OPEN`, `MERGED` or `CLOSED`, as `gh` writes it.
    pub state: String,
    pub updated: String,
    pub title: String,
}

/// A change between two snapshots of one upstream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Head { from: String, to: String },
    Pull { number: u64, what: &'static str, title: String },
}

/// What changed from `before` to `after`. A part `after` lacks (the upstream did not answer) is
/// no change, and a pull request that fell out of the window is not one either.
pub fn changes(before: &Snapshot, after: &Snapshot) -> Vec<Change> {
    let mut out = Vec::new();
    if let (Some(from), Some(to)) = (&before.head, &after.head)
        && from != to
    {
        out.push(Change::Head { from: from.clone(), to: to.clone() });
    }
    for (number, pull) in &after.pulls {
        let old = before.pulls.get(number);
        let what = match (old, pull.state.as_str()) {
            (Some(old), state) if old.state == state => {
                // Comments keep moving a closed one's time; only an open one's push is news.
                if state != "OPEN" || old.updated == pull.updated {
                    continue;
                }
                "updated"
            }
            (_, "MERGED") => "merged",
            (None, "OPEN") => "opened",
            (Some(_), "OPEN") => "reopened",
            _ => "closed",
        };
        out.push(Change::Pull { number: *number, what, title: pull.title.clone() });
    }
    out
}

pub fn run(sh: &Shell, root: &Utf8Path, interval: Duration, once: bool) -> Result<()> {
    let config = Config::load(root)?;
    let mut watched: Vec<Watched<'_>> = config
        .forks()
        .into_iter()
        .map(|(_, fork)| &fork.tracking)
        .map(|t| {
            (t.upstream.as_str(), t.upstream_branch.as_str(), t.base.as_str(), t.paths.as_slice())
        })
        .chain(
            std::iter::once(&config.noq).map(|v| {
                (v.upstream.as_str(), v.upstream_branch.as_str(), v.base.as_str(), &[][..])
            }),
        )
        .filter(|(url, ..)| url.contains("github.com"))
        .map(|(url, branch, base, paths)| Watched {
            slug: repo_slug(url),
            url,
            branch,
            base,
            paths,
        })
        .collect();
    watched.sort_by(|a, b| a.slug.cmp(&b.slug));
    let dir = root.join("target").join("upstream-watch");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {dir}"))?;
    let path = dir.join("state.json");
    let mut state: BTreeMap<String, Snapshot> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let names: Vec<&str> = watched.iter().map(|w| w.slug.as_str()).collect();
    println!("watching {} every {}s", names.join(", "), interval.as_secs());
    loop {
        for source in &watched {
            let after = look(sh, source);
            let known = state.get(&source.slug).cloned();
            if let Some(before) = &known {
                for change in changes(before, &after) {
                    if let Some(line) = describe(sh, source, &change) {
                        println!("{line}");
                    }
                }
            }
            let mut merged = known.unwrap_or_default();
            if after.head.is_some() {
                merged.head = after.head;
            }
            if !after.pulls.is_empty() {
                merged.pulls = after.pulls;
            }
            state.insert(source.slug.clone(), merged);
        }
        let _flushed = std::io::stdout().flush();
        save(&path, &state)?;
        if once {
            return Ok(());
        }
        #[expect(clippy::disallowed_methods, reason = "a CLI's poll loop, not library code")]
        std::thread::sleep(interval);
    }
}

fn save(path: &Utf8PathBuf, state: &BTreeMap<String, Snapshot>) -> Result<()> {
    let staged = path.with_extension("json.new");
    std::fs::write(&staged, serde_json::to_vec_pretty(state)?)
        .with_context(|| format!("write {staged}"))?;
    std::fs::rename(&staged, path).with_context(|| format!("write {path}"))
}

/// A pull request as `gh pr list --json` writes it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    number: u64,
    state: String,
    updated_at: String,
    title: String,
    /// Asked for only where the upstream has `paths`.
    #[serde(default)]
    files: Vec<ListedFile>,
}

#[derive(Deserialize)]
struct ListedFile {
    path: String,
}

/// The upstream now; what it did not answer is left out.
fn look(sh: &Shell, source: &Watched<'_>) -> Snapshot {
    let (url, branch) = (source.url, format!("refs/heads/{}", source.branch));
    let head =
        cmd!(sh, "git -c http.lowSpeedLimit=1000 -c http.lowSpeedTime=10 ls-remote {url} {branch}")
            .quiet()
            .ignore_stderr()
            .read()
            .ok()
            .and_then(|out| out.split_whitespace().next().map(str::to_owned));
    let slug = &source.slug;
    let fields = if source.paths.is_empty() {
        "number,state,updatedAt,title"
    } else {
        "number,state,updatedAt,title,files"
    };
    let pulls = cmd!(sh, "gh pr list -R {slug} --state all --limit {PULLS} --json {fields}")
        .quiet()
        .ignore_stderr()
        .read()
        .ok()
        .and_then(|out| serde_json::from_str::<Vec<Listed>>(&out).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|p| {
            let files: Vec<String> = p.files.iter().map(|f| f.path.clone()).collect();
            relevant(&files, source.paths, PULL_FILES)
        })
        .map(|p| (p.number, Pull { state: p.state, updated: p.updated_at, title: p.title }))
        .collect();
    Snapshot { head, pulls }
}

/// The line for a change, if it is news. A head names its subject and how far it is past the
/// base, when GitHub answers. Under `paths`, a head move is news only when the commits since
/// the last look touch one of them, and then the line names the files.
fn describe(sh: &Shell, source: &Watched<'_>, change: &Change) -> Option<String> {
    let slug = &source.slug;
    match change {
        Change::Head { from, to: head } => {
            // Without an answer from GitHub the move is named: it cannot be ruled out.
            let between = (!source.paths.is_empty())
                .then(|| compare(sh, source.url, from, head).ok())
                .flatten();
            if between.as_ref().is_some_and(|b| !relevant(&b.files, source.paths, COMPARE_FILES)) {
                return None;
            }
            let touched = between.map_or_else(String::new, |between| {
                let files = touching(&between.files, source.paths);
                let named = if files.is_empty() {
                    "more files than GitHub lists".to_owned()
                } else {
                    files.join(" ")
                };
                format!("; {} commits since the last look, touching {named}", between.ahead)
            });
            let subject = cmd!(sh, "gh api repos/{slug}/commits/{head} --jq .commit.message")
                .quiet()
                .ignore_stderr()
                .read()
                .ok()
                .and_then(|m| m.lines().next().map(str::to_owned))
                .unwrap_or_default();
            let range = format!("{}...{head}", source.base);
            let ahead = cmd!(sh, "gh api repos/{slug}/compare/{range} --jq .ahead_by")
                .quiet()
                .ignore_stderr()
                .read()
                .map_or_else(
                    |_| String::new(),
                    |n| format!(" ({} commits past base {})", n.trim(), short(source.base)),
                );
            Some(format!("{slug} head {}: {subject}{ahead}{touched}", short(head)))
        }
        Change::Pull { number, what, title } => {
            Some(format!("{slug} PR #{number} {what}: {title}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Change, Pull, Snapshot, changes};

    fn pull(state: &str, updated: &str, title: &str) -> Pull {
        Pull { state: state.to_owned(), updated: updated.to_owned(), title: title.to_owned() }
    }

    fn snapshot(head: Option<&str>, pulls: &[(u64, Pull)]) -> Snapshot {
        Snapshot { head: head.map(str::to_owned), pulls: pulls.iter().cloned().collect() }
    }

    /// Every kind of change is named once, and nothing is named twice or for a silent upstream.
    #[test]
    fn two_snapshots_differ_by_what_moved() {
        let before = snapshot(
            Some("aaa"),
            &[
                (1, pull("OPEN", "t1", "stays")),
                (2, pull("OPEN", "t1", "gets a push")),
                (3, pull("OPEN", "t1", "lands")),
                (4, pull("OPEN", "t1", "is dropped")),
                (5, pull("CLOSED", "t1", "comes back")),
                (6, pull("MERGED", "t1", "landed long ago")),
                (7, pull("OPEN", "t1", "falls out of the window")),
            ],
        );
        let after = snapshot(
            Some("bbb"),
            &[
                (1, pull("OPEN", "t1", "stays")),
                (2, pull("OPEN", "t2", "gets a push")),
                (3, pull("MERGED", "t2", "lands")),
                (4, pull("CLOSED", "t2", "is dropped")),
                (5, pull("OPEN", "t2", "comes back")),
                (6, pull("MERGED", "t2", "landed long ago")),
                (8, pull("OPEN", "t2", "new")),
                (9, pull("MERGED", "t2", "opened and merged between two looks")),
            ],
        );
        let named: Vec<String> = changes(&before, &after)
            .into_iter()
            .map(|c| match c {
                Change::Head { from, to } => format!("head {from} to {to}"),
                Change::Pull { number, what, .. } => format!("#{number} {what}"),
            })
            .collect();
        assert_eq!(
            named,
            [
                "head aaa to bbb",
                "#2 updated",
                "#3 merged",
                "#4 closed",
                "#5 reopened",
                "#8 opened",
                "#9 merged"
            ]
        );
        assert!(changes(&after, &after).is_empty(), "nothing moved");
        let silent = Snapshot::default();
        assert!(changes(&after, &silent).is_empty(), "an upstream that did not answer");
        assert!(changes(&silent, &after).iter().all(|c| !matches!(c, Change::Head { .. })));
    }
}

//! zed, imported into the gpui-fast fork.
//!
//! gpui-fast is GPUI lifted flat out of zed, with no history in common, and it takes a newer zed
//! by vendor commit (its `docs/upstream-sync.md`, "Syncing with upstream"). The fork's `UPSTREAM`
//! file names the zed commit its copy came from (`zed_commit`), our commit holding that copy
//! unchanged (`import_commit`, the last vendor commit) and the directories that are zed's
//! (`tracked`). longbridge imports zed only now and then, so the fork does it itself:
//!
//! 1. a vendor commit on top of `import_commit` whose tracked directories are zed's, byte for byte,
//!    at the new commit (`zed: import <short>`), built with a scratch index and work tree so
//!    neither checkout's own index or work tree is touched;
//! 2. a merge of it into the fork's branch, with `UPSTREAM` rewritten in the same merge;
//! 3. a stop, mid-merge, for what only a person or an agent can do: conflicts, files zed changed
//!    that a `#[path = "fast/…"]` redirect replaces with our rewrite (the merge cannot see those),
//!    and crates that joined or left the tracked set, which the workspace manifest has to name.
//!
//! The zed checkout lends its objects only: it is fetched, never checked out or switched.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use xshell::{Shell, cmd};

use super::{
    Head, Import, commit_date, fetch, is_partial, remote_for, short, tags_since, today_days,
};

/// The file in the fork that records the import.
const UPSTREAM: &str = "UPSTREAM";

/// What the fork's `UPSTREAM` records.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Recorded {
    /// The zed commit the tracked directories were imported from.
    zed_commit: String,
    /// Our commit holding that import unchanged: the last vendor commit.
    import_commit: String,
    /// The directories that are copies of zed's at the same paths.
    tracked: Vec<String>,
}

impl Recorded {
    fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("parsing the fork's UPSTREAM")
    }

    /// Read at `rev` of the fork.
    fn at(sh: &Shell, fork: &Utf8Path, rev: &str) -> Result<Self> {
        let spec = format!("{rev}:{UPSTREAM}");
        Self::parse(&cmd!(sh, "git -C {fork} show {spec}").quiet().read()?)
    }
}

/// `text` with `zed_commit`, `import_commit` and `tracked` replaced by `recorded`'s, every
/// comment and other line kept in place.
fn rewrite_upstream(text: &str, recorded: &Recorded) -> Result<String> {
    fn key(line: &str) -> Option<&str> {
        let line = line.trim_start();
        if line.starts_with('#') { None } else { line.split_once('=').map(|(key, _)| key.trim()) }
    }
    let mut out = String::with_capacity(text.len());
    let (mut zed, mut import, mut tracked) = (false, false, false);
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        match key(line) {
            Some("zed_commit") => {
                let _written = writeln!(out, "zed_commit = \"{}\"", recorded.zed_commit);
                zed = true;
            }
            Some("import_commit") => {
                let _written = writeln!(out, "import_commit = \"{}\"", recorded.import_commit);
                import = true;
            }
            Some("tracked") => {
                if !line.trim_end().ends_with(']') {
                    ensure!(
                        lines.by_ref().any(|rest| rest.trim_start().starts_with(']')),
                        "UPSTREAM's tracked list has no closing bracket"
                    );
                }
                out.push_str("tracked = [\n");
                for dir in &recorded.tracked {
                    let _written = writeln!(out, "    \"{dir}\",");
                }
                out.push_str("]\n");
                tracked = true;
            }
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    ensure!(zed && import && tracked, "UPSTREAM lacks zed_commit, import_commit or tracked");
    ensure!(Recorded::parse(&out)? == *recorded, "rewriting UPSTREAM lost a value");
    Ok(out)
}

/// The tracked set at a new zed commit.
#[derive(Debug, PartialEq, Eq)]
struct Tracked {
    /// The directories to import: the old ones zed still has, plus the crates the tracked crates
    /// now depend on that none of them covers.
    dirs: Vec<String>,
    /// Every crate directory the tracked crates depend on (normal, dev and build dependencies,
    /// any target), themselves included: what the fork's workspace has to list as members.
    crates: BTreeSet<String>,
    /// Directories that joined the set.
    added: Vec<String>,
    /// Directories zed no longer has.
    removed: Vec<String>,
}

/// Work out [`Tracked`] from zed's workspace manifest and `manifest`, which gives a crate
/// directory's `Cargo.toml` at the new commit (`None` when zed has no such directory). Each
/// tracked directory is a crate directory; a crate inside one (`crates/refineable/
/// derive_refineable`) is covered by it.
fn next_tracked(
    old: &[String],
    workspace: &str,
    mut manifest: impl FnMut(&str) -> Result<Option<String>>,
) -> Result<Tracked> {
    let workspace: toml::Table = toml::from_str(workspace).context("parsing zed's Cargo.toml")?;
    let members: BTreeMap<&str, String> = workspace
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
        .filter_map(|(name, spec)| {
            let path = spec.as_table()?.get("path")?.as_str()?;
            Some((name.as_str(), join("", path)))
        })
        .collect();

    let mut kept = Vec::new();
    let mut removed = Vec::new();
    let mut queue = Vec::new();
    let mut manifests = BTreeMap::new();
    for dir in old {
        match manifest(dir)? {
            Some(text) => {
                kept.push(dir.clone());
                manifests.insert(dir.clone(), text);
                queue.push(dir.clone());
            }
            None => removed.push(dir.clone()),
        }
    }
    let mut crates: BTreeSet<String> = queue.iter().cloned().collect();
    while let Some(dir) = queue.pop() {
        let text = match manifests.remove(&dir) {
            Some(text) => text,
            None => manifest(&dir)?.with_context(|| {
                format!("zed's crates depend on {dir}, which has no Cargo.toml")
            })?,
        };
        let parsed: toml::Table =
            toml::from_str(&text).with_context(|| format!("parsing zed's {dir}/Cargo.toml"))?;
        for dep in path_dependencies(&dir, &parsed, &members) {
            if crates.insert(dep.clone()) {
                queue.push(dep);
            }
        }
    }
    let mut dirs = kept.clone();
    for dir in &crates {
        if !covered(dir, &dirs) {
            dirs.push(dir.clone());
        }
    }
    dirs.sort();
    let added = dirs.iter().filter(|dir| !old.contains(dir)).cloned().collect();
    Ok(Tracked { dirs, crates, added, removed })
}

/// The crate directories a manifest names by path: directly, or through a workspace dependency
/// that has one.
fn path_dependencies(
    dir: &str,
    manifest: &toml::Table,
    workspace: &BTreeMap<&str, String>,
) -> Vec<String> {
    const KINDS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    let tables = |root: &toml::Table| -> Vec<toml::Table> {
        KINDS.iter().filter_map(|kind| root.get(*kind)?.as_table().cloned()).collect()
    };
    let mut all = tables(manifest);
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values().filter_map(toml::Value::as_table) {
            all.extend(tables(target));
        }
    }
    all.iter()
        .flat_map(toml::Table::iter)
        .filter_map(|(name, spec)| {
            let spec = spec.as_table()?;
            if spec.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                workspace.get(name.as_str()).cloned()
            } else {
                spec.get("path")?.as_str().map(|path| join(dir, path))
            }
        })
        .collect()
}

/// `rel` resolved against the directory `base`, both `/`-separated and relative to a root.
fn join(base: &str, rel: &str) -> String {
    let mut parts: Vec<&str> =
        base.split('/').filter(|part| !part.is_empty() && *part != ".").collect();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

/// Whether `dir` is one of `roots` or inside one.
fn covered(dir: &str, roots: &[String]) -> bool {
    roots.iter().any(|root| {
        dir.strip_prefix(root.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// What the fork's workspace manifest still has to change: crates of the tracked set it does not
/// list as members, and members inside directories zed dropped.
fn unwired(members: &[String], tracked: &Tracked) -> (Vec<String>, Vec<String>) {
    let missing = tracked.crates.iter().filter(|dir| !members.contains(dir)).cloned().collect();
    let stale =
        members.iter().filter(|member| covered(member, &tracked.removed)).cloned().collect();
    (missing, stale)
}

/// The `[workspace] members` of a manifest.
fn workspace_members(manifest: &str) -> Result<Vec<String>> {
    let parsed: toml::Table = toml::from_str(manifest).context("parsing the fork's Cargo.toml")?;
    Ok(parsed
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|member| member.as_str().map(|m| join("", m)))
        .collect())
}

/// A `#[path = "…"] mod name;` in a source file.
#[derive(Debug, PartialEq, Eq)]
struct Redirect {
    /// The attribute's path, relative to the declaring file's directory.
    target: String,
    /// The module it loads.
    module: String,
}

impl Redirect {
    /// Whether it loads one of our rewrites in place of an upstream file.
    fn is_fast(&self) -> bool {
        self.target.split('/').any(|part| part == "fast")
    }
}

/// Every file-module `#[path]` redirect in `source`, skipping line comments. Other attributes and
/// comments may sit between the attribute and the `mod`, and the `mod` may carry a visibility.
fn path_redirects(source: &str) -> Vec<Redirect> {
    source
        .match_indices("#[path")
        .filter(|(at, _)| {
            source
                .get(..*at)
                .and_then(|before| before.rsplit('\n').next())
                .is_some_and(|line| !line.contains("//"))
        })
        .filter_map(|(at, _)| redirect_at(source.get(at..)?))
        .collect()
}

fn redirect_at(text: &str) -> Option<Redirect> {
    let rest = text.strip_prefix("#[path")?.trim_start().strip_prefix('=')?;
    let (target, rest) = rest.trim_start().strip_prefix('"')?.split_once('"')?;
    let mut rest = rest.trim_start().strip_prefix(']')?;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.split_once('\n').map_or("", |(_, next)| next);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, next)| next);
        } else if rest.starts_with("#[") {
            rest = skip_attribute(rest)?;
        } else {
            break;
        }
    }
    if let Some(after) = rest.strip_prefix("pub") {
        let after = after.trim_start();
        rest = after
            .strip_prefix('(')
            .map_or(Some(after), |inner| inner.split_once(')').map(|(_, r)| r))?;
        rest = rest.trim_start();
    }
    let rest = rest.strip_prefix("mod")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("r#").unwrap_or(rest);
    let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    let (module, tail) = rest.split_at_checked(end)?;
    (!module.is_empty() && tail.trim_start().starts_with(';'))
        .then(|| Redirect { target: target.to_owned(), module: module.to_owned() })
}

/// `text` past the attribute it starts with (`#[…]`, brackets balanced).
fn skip_attribute(text: &str) -> Option<&str> {
    let mut depth = 0_usize;
    let close = text.char_indices().find(|&(_, c)| {
        match c {
            '[' => depth = depth.saturating_add(1),
            ']' => depth = depth.saturating_sub(1),
            _ => return false,
        }
        depth == 0
    })?;
    text.get(close.0..)?.strip_prefix(']')
}

/// Where the module an upstream file declares lives when nothing redirects it, most likely
/// first. A `lib.rs`, `main.rs` or `mod.rs` declares its modules beside itself; any other file
/// in a directory named after itself, unless it is a crate root (`src/gpui.rs`), which is why
/// both are candidates and the caller keeps the first that upstream has.
fn original_candidates(declaring: &str, module: &str) -> Vec<String> {
    let path = Utf8Path::new(declaring);
    let dir = path.parent().unwrap_or_else(|| Utf8Path::new(""));
    let beside = [dir.join(format!("{module}.rs")), dir.join(module).join("mod.rs")];
    let mut candidates = Vec::new();
    if let Some(stem) = path.file_stem().filter(|stem| !["lib", "main", "mod"].contains(stem)) {
        let nested = dir.join(stem);
        candidates.push(nested.join(format!("{module}.rs")));
        candidates.push(nested.join(module).join("mod.rs"));
    }
    candidates.extend(beside);
    candidates.into_iter().map(Utf8PathBuf::into_string).collect()
}

/// An upstream file we replaced with a rewrite of our own.
#[derive(Debug, PartialEq, Eq)]
struct Port {
    /// Upstream's file, which stays in the tree unused.
    original: String,
    /// Our rewrite the redirect loads.
    copy: String,
}

/// The `fast/` redirects in the tracked directories of the fork at `rev`, each with the upstream
/// file it replaces: the first candidate that `rev_with_upstream` (a vendor commit) has.
fn redirects(
    sh: &Shell,
    fork: &Utf8Path,
    rev: &str,
    dirs: &[String],
    rev_with_upstream: &str,
) -> Result<Vec<Port>> {
    let out = cmd!(sh, "git -C {fork} grep -l -F -e #[path {rev} -- {dirs...}")
        .quiet()
        .ignore_status()
        .output()?;
    ensure!(
        matches!(out.status.code(), Some(0 | 1)),
        "git grep failed in {fork}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8(out.stdout).context("git grep printed non-UTF-8")?;
    let prefix = format!("{rev}:");
    let mut ports = Vec::new();
    for file in listing.lines().filter_map(|line| line.strip_prefix(&prefix)) {
        if Utf8Path::new(file).extension() != Some("rs") {
            continue;
        }
        let spec = format!("{rev}:{file}");
        let source = cmd!(sh, "git -C {fork} show {spec}").quiet().read()?;
        for redirect in path_redirects(&source).into_iter().filter(Redirect::is_fast) {
            let found = original_candidates(file, &redirect.module).into_iter().find(|candidate| {
                let spec = format!("{rev_with_upstream}:{candidate}");
                cmd!(sh, "git -C {fork} cat-file -e {spec}").quiet().ignore_stderr().run().is_ok()
            });
            if let Some(original) = found {
                let dir =
                    Utf8Path::new(file).parent().map_or_else(String::new, Utf8Path::to_string);
                ports.push(Port { original, copy: join(&dir, &redirect.target) });
            }
        }
    }
    Ok(ports)
}

/// How `path` changed between two commits of `repo` (`+added −removed`), or `None` when it did
/// not.
fn change(sh: &Shell, repo: &Utf8Path, from: &str, to: &str, path: &str) -> Result<Option<String>> {
    let out = cmd!(sh, "git -C {repo} diff --numstat {from} {to} -- {path}").quiet().read()?;
    Ok(out.lines().next().map(|line| {
        let mut fields = line.split('\t');
        match (fields.next(), fields.next()) {
            (Some(added), Some(removed)) => format!("+{added} −{removed}"),
            _ => line.to_owned(),
        }
    }))
}

/// How many commits in `from..to` touch `dirs` (all commits when `dirs` is empty).
fn ahead(sh: &Shell, repo: &Utf8Path, from: &str, to: &str, dirs: &[String]) -> Result<u64> {
    let range = format!("{from}..{to}");
    let count = cmd!(sh, "git -C {repo} rev-list --count {range} -- {dirs...}").quiet().read()?;
    count.trim().parse().with_context(|| format!("git rev-list --count printed {count:?}"))
}

/// What a sync does with zed.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// The fork already holds everything zed changed in the tracked directories.
    Current,
    /// Import zed's head.
    Import,
}

fn plan(recorded: &Recorded, head: &str, ahead: u64) -> Plan {
    if recorded.zed_commit == head || ahead == 0 { Plan::Current } else { Plan::Import }
}

/// Fetch zed's branch into the zed checkout, and the recorded commit if the checkout lacks it.
fn fetch_zed(sh: &Shell, zed: &Import, zed_dir: &Utf8Path, recorded: &str) -> Result<Head> {
    let _dir = sh.push_dir(zed_dir);
    let head = fetch(sh, &zed.tracking.upstream, &zed.tracking.upstream_branch)?;
    let object = format!("{recorded}^{{commit}}");
    if cmd!(sh, "git cat-file -e {object}").quiet().ignore_stderr().run().is_err() {
        let remote = remote_for(sh, &zed.tracking.upstream)?;
        let filter: &[&str] = if is_partial(sh) { &["--filter=blob:none"] } else { &[] };
        cmd!(sh, "git fetch --quiet --no-tags {filter...} {remote} {recorded}")
            .quiet()
            .run()
            .with_context(|| format!("fetching zed {recorded}, which the fork's UPSTREAM names"))?;
    }
    let head_sha = &head.sha;
    ensure!(
        cmd!(sh, "git merge-base --is-ancestor {recorded} {head_sha}").quiet().run().is_ok(),
        "zed {} is not an ancestor of zed {} {}",
        short(recorded),
        zed.tracking.upstream_branch,
        short(head_sha)
    );
    Ok(head)
}

/// In a blobless zed clone, fetch every `Cargo.toml` at `rev` in one go, so the dependency walk
/// does not fetch them one at a time.
fn prefetch_manifests(sh: &Shell, zed: &Import, zed_dir: &Utf8Path, rev: &str) -> Result<()> {
    let _dir = sh.push_dir(zed_dir);
    if !is_partial(sh) {
        return Ok(());
    }
    let listing = cmd!(sh, "git ls-tree -r {rev}").quiet().read()?;
    // `<mode> <type> <oid>\t<path>`
    let wanted = lines_of(
        listing
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .filter(|(_, path)| *path == "Cargo.toml" || path.ends_with("/Cargo.toml"))
            .filter_map(|(meta, _)| meta.split(' ').nth(2)),
    );
    let present = cmd!(sh, "git cat-file --batch-check")
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdin(&wanted)
        .quiet()
        .read()?;
    let missing = lines_of(present.lines().filter_map(|line| line.strip_suffix(" missing")));
    if missing.is_empty() {
        return Ok(());
    }
    let remote = remote_for(sh, &zed.tracking.upstream)?;
    // What git itself runs to fill a partial clone, with the whole batch at once.
    cmd!(
        sh,
        "git -c fetch.negotiationAlgorithm=noop fetch --quiet --no-tags --no-write-fetch-head --recurse-submodules=no --filter=blob:none --stdin {remote}"
    )
    .stdin(&missing)
    .quiet()
    .run()
    .context("fetching zed's manifests")
}

/// One line per item, for a git command's standard input.
fn lines_of<'a>(items: impl Iterator<Item = &'a str>) -> String {
    items.fold(String::new(), |mut out, item| {
        out.push_str(item);
        out.push('\n');
        out
    })
}

/// [`next_tracked`] at `rev` of the zed checkout.
fn tracked_at(
    sh: &Shell,
    zed: &Import,
    zed_dir: &Utf8Path,
    rev: &str,
    old: &[String],
) -> Result<Tracked> {
    prefetch_manifests(sh, zed, zed_dir, rev)?;
    let root = format!("{rev}:Cargo.toml");
    let workspace = cmd!(sh, "git -C {zed_dir} show {root}").quiet().read()?;
    next_tracked(old, &workspace, |dir| {
        let spec = format!("{rev}:{dir}/Cargo.toml");
        Ok(cmd!(sh, "git -C {zed_dir} show {spec}").quiet().ignore_stderr().read().ok())
    })
}

/// `cargo xtask upstream check` for zed: how far zed is ahead of the fork's import, and what an
/// import would ask for by hand.
pub(super) fn check(
    sh: &Shell,
    zed: &Import,
    zed_dir: &Utf8Path,
    fork: &Utf8Path,
    fork_head: &str,
    longbridge_head: &str,
) -> Result<()> {
    let recorded = Recorded::at(sh, fork, fork_head)?;
    let head = fetch_zed(sh, zed, zed_dir, &recorded.zed_commit)?;
    let behind = ahead(sh, zed_dir, &recorded.zed_commit, &head.sha, &recorded.tracked)?;
    let behind_all = ahead(sh, zed_dir, &recorded.zed_commit, &head.sha, &[])?;
    let date = {
        let _dir = sh.push_dir(zed_dir);
        commit_date(sh, &recorded.zed_commit).unwrap_or_default()
    };
    let age = zed.tracking.days_since_current(today_days()?)?;
    println!(
        "zed → gpui-fast: the fork imports {} ({date}, current {age} days ago); zed {} at {} \
         ({}): {behind} commits ahead in the tracked directories ({behind_all} in all)",
        short(&recorded.zed_commit),
        zed.tracking.upstream_branch,
        short(&head.sha),
        head.date,
    );
    {
        let _dir = sh.push_dir(zed_dir);
        let tags = tags_since(sh, &recorded.zed_commit, &head.sha)?;
        if !tags.is_empty() {
            println!("  tags since the import: {}", tags.join(", "));
        }
        if let Ok(theirs) = Recorded::at(sh, fork, longbridge_head)
            && theirs.zed_commit != recorded.zed_commit
        {
            let theirs_date = commit_date(sh, &theirs.zed_commit).unwrap_or_default();
            println!("  longbridge's main imports {} ({theirs_date})", short(&theirs.zed_commit));
        }
    }
    if plan(&recorded, &head.sha, behind) == Plan::Current {
        return Ok(());
    }
    let tracked = tracked_at(sh, zed, zed_dir, &head.sha, &recorded.tracked)?;
    if !tracked.added.is_empty() || !tracked.removed.is_empty() {
        let changes: Vec<String> = tracked
            .added
            .iter()
            .map(|dir| format!("+{dir}"))
            .chain(tracked.removed.iter().map(|dir| format!("−{dir}")))
            .collect();
        println!("  tracked at zed {}: {}", zed.tracking.upstream_branch, changes.join(", "));
    }
    let manifest = format!("{fork_head}:Cargo.toml");
    let members = workspace_members(&cmd!(sh, "git -C {fork} show {manifest}").quiet().read()?)?;
    let (missing, stale) = unwired(&members, &tracked);
    if !missing.is_empty() {
        println!("  the fork's workspace would have to add: {}", missing.join(", "));
    }
    if !stale.is_empty() {
        println!("  the fork's workspace would have to drop: {}", stale.join(", "));
    }
    let every = union(&recorded.tracked, &tracked.dirs);
    let mut changed = Vec::new();
    for port in redirects(sh, fork, fork_head, &every, &recorded.import_commit)? {
        if let Some(stat) = change(sh, zed_dir, &recorded.zed_commit, &head.sha, &port.original)? {
            changed.push(format!("{} ({stat}) → {}", port.original, port.copy));
        }
    }
    if !changed.is_empty() {
        println!("  redirected files zed changed since the import (ported by hand on sync):");
        for line in changed {
            println!("    {line}");
        }
    }
    Ok(())
}

fn union(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().chain(b).cloned().collect::<BTreeSet<_>>().into_iter().collect()
}

/// Import zed's head into the fork's `branch` (checked out in `fork`, clean) and return the zed
/// head the fork is now current with. Stops mid-merge, with the list, when the import needs a
/// hand; `UPSTREAM` is then already rewritten and staged, so committing the merge finishes it.
pub(super) fn sync(
    sh: &Shell,
    zed: &Import,
    zed_dir: &Utf8Path,
    fork: &Utf8Path,
    branch: &str,
) -> Result<Head> {
    println!("▶ zed → gpui-fast: {zed_dir}");
    let recorded = Recorded::at(sh, fork, branch)?;
    let head = fetch_zed(sh, zed, zed_dir, &recorded.zed_commit)?;
    let behind = ahead(sh, zed_dir, &recorded.zed_commit, &head.sha, &recorded.tracked)?;
    if plan(&recorded, &head.sha, behind) == Plan::Current {
        println!(
            "  the fork's import of {} holds everything zed {} {} changed in its directories",
            short(&recorded.zed_commit),
            zed.tracking.upstream_branch,
            short(&head.sha)
        );
        return Ok(head);
    }
    let tracked = tracked_at(sh, zed, zed_dir, &head.sha, &recorded.tracked)?;
    let vendor = vendor_commit(sh, zed_dir, fork, &recorded, &tracked, &head)?;
    println!(
        "  vendor commit {} imports zed {} ({behind} commits in the tracked directories)",
        short(&vendor),
        short(&head.sha)
    );

    cmd!(sh, "git -C {fork} switch --quiet {branch}").run()?;
    let message = format!("zed: merge the import of {}", head.sha.get(..8).unwrap_or(&head.sha));
    let clean =
        cmd!(sh, "git -C {fork} merge --no-ff --no-commit -m {message} {vendor}").run().is_ok();
    ensure!(
        cmd!(sh, "git -C {fork} rev-parse -q --verify MERGE_HEAD")
            .quiet()
            .ignore_stdout()
            .run()
            .is_ok(),
        "merging the vendor commit {vendor} into {branch} in {fork} did not start; see git's output"
    );
    let path = fork.join(UPSTREAM);
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
    let next = Recorded {
        zed_commit: head.sha.clone(),
        import_commit: vendor.clone(),
        tracked: tracked.dirs.clone(),
    };
    std::fs::write(&path, rewrite_upstream(&text, &next)?)
        .with_context(|| format!("writing {path}"))?;
    cmd!(sh, "git -C {fork} add {UPSTREAM}").run()?;

    let conflicts: Vec<String> = cmd!(sh, "git -C {fork} diff --name-only --diff-filter=U")
        .quiet()
        .read()?
        .lines()
        .map(str::to_owned)
        .collect();
    let every = union(&recorded.tracked, &tracked.dirs);
    let mut ports = Vec::new();
    for port in redirects(sh, fork, "HEAD", &every, &recorded.import_commit)? {
        if let Some(stat) = change(sh, fork, &recorded.import_commit, &vendor, &port.original)? {
            ports.push((port, stat));
        }
    }
    let manifest = std::fs::read_to_string(fork.join("Cargo.toml"))
        .context("reading the fork's Cargo.toml")?;
    let (missing, stale) = unwired(&workspace_members(&manifest)?, &tracked);
    let work = HandWork { conflicts, ports, missing, stale };
    if work.is_empty() {
        cmd!(sh, "git -C {fork} commit --quiet --no-edit").run()?;
        println!("  merged {}{}", short(&vendor), if clean { "" } else { " (conflicts resolved)" });
        return Ok(head);
    }
    bail!("{}", work.report(fork, &recorded.import_commit, &vendor))
}

/// What an import leaves to a person or an agent.
#[derive(Debug)]
struct HandWork {
    conflicts: Vec<String>,
    ports: Vec<(Port, String)>,
    missing: Vec<String>,
    stale: Vec<String>,
}

impl HandWork {
    const fn is_empty(&self) -> bool {
        self.conflicts.is_empty()
            && self.ports.is_empty()
            && self.missing.is_empty()
            && self.stale.is_empty()
    }

    fn report(&self, fork: &Utf8Path, old: &str, new: &str) -> String {
        let mut out = format!(
            "the zed import stopped in {fork}, mid-merge with UPSTREAM already rewritten and staged; \
             finish it there:"
        );
        if !self.conflicts.is_empty() {
            let _written = write!(
                out,
                "\n  conflicts (keep upstream's code and put our hook back): {}",
                self.conflicts.join(", ")
            );
        }
        if !self.ports.is_empty() {
            out.push_str(
                "\n  port by hand: zed changed these files, which a `#[path = \"fast/…\"]` \
                 redirect replaces with our rewrite, so the merge could not carry the change:",
            );
            for (port, stat) in &self.ports {
                let _written = write!(
                    out,
                    "\n    {} ({stat}) → {}: git diff {} {} -- {}",
                    port.original,
                    port.copy,
                    short(old),
                    short(new),
                    port.original
                );
            }
        }
        if !self.missing.is_empty() {
            let _written = write!(
                out,
                "\n  add to Cargo.toml's [workspace] members and [workspace.dependencies] (the \
                 tracked crates now depend on them): {}",
                self.missing.join(", ")
            );
        }
        if !self.stale.is_empty() {
            let _written = write!(
                out,
                "\n  remove from Cargo.toml (zed no longer has them): {}",
                self.stale.join(", ")
            );
        }
        out.push_str(
            "\n  then `git commit` the merge there and run `cargo xtask upstream sync --only \
             gpui-fast` again",
        );
        out
    }
}

/// Build the vendor commit: `recorded.import_commit` with every tracked directory replaced by
/// zed's at `head`, byte for byte, through a scratch index and work tree under the fork's git
/// directory.
fn vendor_commit(
    sh: &Shell,
    zed_dir: &Utf8Path,
    fork: &Utf8Path,
    recorded: &Recorded,
    tracked: &Tracked,
    head: &Head,
) -> Result<String> {
    let git_dir = |repo: &Utf8Path| -> Result<Utf8PathBuf> {
        Ok(cmd!(sh, "git -C {repo} rev-parse --absolute-git-dir").quiet().read()?.into())
    };
    let (zed_git, fork_git) = (git_dir(zed_dir)?, git_dir(fork)?);
    let scratch = fork_git.join("xtask-zed-import");
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).with_context(|| format!("clearing {scratch}"))?;
    }
    let tree = scratch.join("tree");
    std::fs::create_dir_all(&tree).with_context(|| format!("creating {tree}"))?;
    let (zed_index, index) = (scratch.join("zed.index"), scratch.join("index"));
    let (sha, dirs, import) = (&head.sha, &tracked.dirs, &recorded.import_commit);
    let every = &union(&recorded.tracked, dirs);
    let tree_sha = {
        let _dir = sh.push_dir(&tree);
        cmd!(sh, "git --git-dir={zed_git} --work-tree={tree} checkout {sha} -- {dirs...}")
            .env("GIT_INDEX_FILE", &zed_index)
            .quiet()
            .run()
            .context("writing zed's directories out")?;
        let git = |args: &[&str]| {
            cmd!(sh, "git --git-dir={fork_git} --work-tree={tree} {args...}")
                .env("GIT_INDEX_FILE", &index)
                .quiet()
        };
        git(&["read-tree", import]).run()?;
        // `-f`: HEAD is the fork's branch, whose own commits differ from the import in these
        // directories, and a plain `rm --cached` refuses an entry unlike both HEAD and the file.
        let mut remove = vec!["rm", "-r", "-q", "-f", "--cached", "--ignore-unmatch", "--"];
        remove.extend(every.iter().map(String::as_str));
        git(&remove).run()?;
        let mut add = vec!["add", "-f", "--"];
        add.extend(dirs.iter().map(String::as_str));
        git(&add).run()?;
        git(&["write-tree"]).read()?
    };
    let ours = cmd!(sh, "git -C {fork} ls-tree -r {tree_sha} -- {every...}").quiet().read()?;
    let theirs = cmd!(sh, "git -C {zed_dir} ls-tree -r {sha} -- {every...}").quiet().read()?;
    ensure!(
        ours == theirs,
        "the vendor tree {tree_sha} differs from zed {sha} in the tracked directories (scratch \
         kept in {scratch})"
    );
    std::fs::remove_dir_all(&scratch).with_context(|| format!("removing {scratch}"))?;

    let subject = format!("zed: import {}", sha.get(..8).unwrap_or(sha));
    let mut body = format!(
        "The upstream directories replaced with zed-industries/zed at\n{sha}, byte for byte."
    );
    if !tracked.added.is_empty() {
        let _written = write!(body, " Added: {}.", tracked.added.join(", "));
    }
    if !tracked.removed.is_empty() {
        let _written =
            write!(body, " Removed, as zed no longer has them: {}.", tracked.removed.join(", "));
    }
    Ok(cmd!(sh, "git -C {fork} commit-tree {tree_sha} -p {import} -m {subject} -m {body}")
        .quiet()
        .read()?)
}

#[cfg(test)]
mod tests {
    use super::super::Tracking;
    use super::*;

    const UPSTREAM_TEXT: &str = "# Where it comes from.\n\
        zed_repository = \"https://github.com/zed-industries/zed\"\n\
        zed_commit = \"aaa\"\n\n\
        # Our commit.\n\
        import_commit = \"bbb\"\n\n\
        # Directories.\n\
        tracked = [\n    \"crates/gpui\",\n    \"crates/media\",\n]\n";

    #[test]
    fn upstream_is_parsed_and_rewritten_in_place() {
        let recorded = Recorded::parse(UPSTREAM_TEXT).expect("parse");
        assert_eq!(
            recorded,
            Recorded {
                zed_commit: "aaa".to_owned(),
                import_commit: "bbb".to_owned(),
                tracked: vec!["crates/gpui".to_owned(), "crates/media".to_owned()],
            },
            "the three values"
        );
        let next = Recorded {
            zed_commit: "ccc".to_owned(),
            import_commit: "ddd".to_owned(),
            tracked: vec!["crates/bench_metrics".to_owned(), "crates/gpui".to_owned()],
        };
        let text = rewrite_upstream(UPSTREAM_TEXT, &next).expect("rewrite");
        assert_eq!(
            text,
            "# Where it comes from.\n\
             zed_repository = \"https://github.com/zed-industries/zed\"\n\
             zed_commit = \"ccc\"\n\n\
             # Our commit.\n\
             import_commit = \"ddd\"\n\n\
             # Directories.\n\
             tracked = [\n    \"crates/bench_metrics\",\n    \"crates/gpui\",\n]\n",
            "comments and order stay"
        );
        assert!(rewrite_upstream("zed_commit = \"a\"\n", &next).is_err(), "missing keys");
        assert!(Recorded::parse("zed_commit = 1").is_err(), "not an UPSTREAM");
    }

    #[test]
    fn only_a_moved_tracked_directory_asks_for_an_import() {
        let recorded = Recorded::parse(UPSTREAM_TEXT).expect("parse");
        assert_eq!(plan(&recorded, "aaa", 0), Plan::Current, "the same commit");
        assert_eq!(plan(&recorded, "eee", 0), Plan::Current, "zed moved elsewhere only");
        assert_eq!(plan(&recorded, "eee", 3), Plan::Import, "zed changed a tracked directory");
    }

    fn manifests(files: &[(&str, &str)]) -> impl FnMut(&str) -> Result<Option<String>> {
        let files: BTreeMap<String, String> =
            files.iter().map(|(dir, text)| ((*dir).to_owned(), (*text).to_owned())).collect();
        move |dir| Ok(files.get(dir).cloned())
    }

    #[test]
    fn the_tracked_set_follows_the_dependencies() {
        let workspace = "[workspace.dependencies]\n\
            gpui = { path = \"crates/gpui\" }\n\
            bench_metrics = { path = \"crates/bench_metrics\" }\n\
            refineable = { path = \"crates/refineable\" }\n\
            derive_refineable = { path = \"crates/refineable/derive_refineable\" }\n\
            perf = { path = \"tooling/perf\" }\n\
            serde = \"1\"\n";
        let old: Vec<String> = ["crates/gpui", "crates/media", "crates/refineable"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let tracked = next_tracked(
            &old,
            workspace,
            manifests(&[
                (
                    "crates/gpui",
                    "[dependencies]\nserde.workspace = true\nrefineable.workspace = true\n\
                     [target.'cfg(target_os = \"macos\")'.dependencies]\n\
                     bench_metrics = { workspace = true, optional = true }\n\
                     [dev-dependencies]\nperf = { workspace = true }\n",
                ),
                ("crates/refineable", "[dependencies]\nderive_refineable.workspace = true\n"),
                ("crates/refineable/derive_refineable", "[dependencies]\n"),
                ("crates/bench_metrics", "[dependencies]\nlocal = { path = \"../local\" }\n"),
                ("crates/local", "[package]\nname = \"local\"\n"),
                ("tooling/perf", "[package]\nname = \"perf\"\n"),
            ]),
        )
        .expect("closure");
        assert_eq!(
            tracked.dirs,
            [
                "crates/bench_metrics",
                "crates/gpui",
                "crates/local",
                "crates/refineable",
                "tooling/perf"
            ],
            "media is gone, new dependencies join, a nested crate is covered"
        );
        assert_eq!(
            tracked.added,
            ["crates/bench_metrics", "crates/local", "tooling/perf"],
            "added"
        );
        assert_eq!(tracked.removed, ["crates/media"], "removed");
        assert!(tracked.crates.contains("crates/refineable/derive_refineable"), "nested crate");

        let members: Vec<String> = [
            "crates/gpui",
            "crates/media",
            "crates/refineable",
            "crates/gpui_perf",
            "tooling/perf",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let (missing, stale) = unwired(&members, &tracked);
        assert_eq!(
            missing,
            ["crates/bench_metrics", "crates/local", "crates/refineable/derive_refineable"],
            "crates the workspace does not list"
        );
        assert_eq!(stale, ["crates/media"], "a member zed dropped");

        let broken = next_tracked(
            &old,
            workspace,
            manifests(&[("crates/gpui", "[dependencies]\nrefineable.workspace = true\n")]),
        );
        assert!(broken.is_err(), "a dependency without a manifest is an error, not a skip");
    }

    #[test]
    fn workspace_members_are_read_and_normalised() {
        let members = workspace_members("[workspace]\nmembers = [\"crates/a\", \"./crates/b/\"]\n")
            .expect("members");
        assert_eq!(members, ["crates/a", "crates/b"], "normalised");
        assert_eq!(join("crates/a", "../b/./c"), "crates/b/c", "parent and current dirs");
        assert!(covered("crates/a/nested", &["crates/a".to_owned()]), "inside");
        assert!(!covered("crates/ab", &["crates/a".to_owned()]), "a prefix is not a parent");
    }

    #[test]
    fn fast_redirects_are_found() {
        let source = "//! Docs mention #[path = \"fast/no.rs\"] mod no; in a comment.\n\
            // gpui-fast replaces the bounds tree.\n\
            #[path = \"fast/bounds_tree.rs\"]\n\
            mod bounds_tree;\n\
            mod color;\n\
            #[path = \"fast/layout.rs\"] // ours\n\
            #[cfg(not(test))]\n\
            pub(crate) mod r#layout;\n\
            #[path = \"example_support/fonts.rs\"]\n\
            mod fonts;\n\
            #[path = \"fast/inline.rs\"]\n\
            mod inline { }\n";
        let found = path_redirects(source);
        assert_eq!(
            found,
            [
                Redirect {
                    target: "fast/bounds_tree.rs".to_owned(),
                    module: "bounds_tree".to_owned()
                },
                Redirect { target: "fast/layout.rs".to_owned(), module: "layout".to_owned() },
                Redirect {
                    target: "example_support/fonts.rs".to_owned(),
                    module: "fonts".to_owned()
                },
            ],
            "file modules only, comments skipped"
        );
        let fast: Vec<&str> =
            found.iter().filter(|r| r.is_fast()).map(|r| r.module.as_str()).collect();
        assert_eq!(fast, ["bounds_tree", "layout"], "only fast/ redirects are ports");
    }

    #[test]
    fn the_original_of_a_redirect_is_where_rust_would_look() {
        assert_eq!(
            original_candidates("crates/gpui/src/gpui.rs", "bounds_tree"),
            [
                "crates/gpui/src/gpui/bounds_tree.rs",
                "crates/gpui/src/gpui/bounds_tree/mod.rs",
                "crates/gpui/src/bounds_tree.rs",
                "crates/gpui/src/bounds_tree/mod.rs",
            ],
            "a crate root named after the crate: nested first, then beside"
        );
        assert_eq!(
            original_candidates("crates/util/src/lib.rs", "paths"),
            ["crates/util/src/paths.rs", "crates/util/src/paths/mod.rs"],
            "lib.rs declares beside itself"
        );
    }

    /// A shell whose git ignores the user's configuration (signing, hooks, default branch).
    fn git_shell(root: &Utf8Path) -> Shell {
        let sh = Shell::new().expect("shell");
        sh.change_dir(root);
        sh.set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        sh.set_var("GIT_CONFIG_NOSYSTEM", "1");
        for (key, value) in [
            ("GIT_AUTHOR_NAME", "test"),
            ("GIT_AUTHOR_EMAIL", "test@example.com"),
            ("GIT_COMMITTER_NAME", "test"),
            ("GIT_COMMITTER_EMAIL", "test@example.com"),
        ] {
            sh.set_var(key, value);
        }
        sh
    }

    fn scratch(name: &str) -> Utf8PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-zed-{name}-{}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp dir");
        Utf8PathBuf::from_path_buf(dir).expect("utf8 tmp dir")
    }

    fn write(root: &Utf8Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, text).expect("write");
    }

    fn commit(sh: &Shell, repo: &Utf8Path, message: &str) -> String {
        cmd!(sh, "git -C {repo} add -A").quiet().run().expect("add");
        cmd!(sh, "git -C {repo} commit -q -m {message}").quiet().run().expect("commit");
        cmd!(sh, "git -C {repo} rev-parse HEAD").quiet().read().expect("head")
    }

    #[test]
    fn commits_ahead_count_only_the_tracked_directories() {
        let root = scratch("ahead");
        let sh = git_shell(&root);
        cmd!(sh, "git init -q -b main {root}").run().expect("init");
        write(&root, "crates/a/lib.rs", "1");
        let from = commit(&sh, &root, "one");
        write(&root, "crates/a/lib.rs", "2");
        commit(&sh, &root, "two");
        write(&root, "docs/x.md", "doc");
        commit(&sh, &root, "three");
        write(&root, "crates/b/lib.rs", "b");
        let to = commit(&sh, &root, "four");
        let dirs = ["crates/a".to_owned()];
        assert_eq!(ahead(&sh, &root, &from, &to, &dirs).ok(), Some(1), "one touches crates/a");
        assert_eq!(ahead(&sh, &root, &from, &to, &[]).ok(), Some(3), "three in all");
        assert_eq!(ahead(&sh, &root, &to, &to, &dirs).ok(), Some(0), "none past the head");
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    /// The whole import on local repositories: a zed with a crate the fork redirects a file of,
    /// a fork holding an old import, and two zed commits, one that needs a hand and one that
    /// does not.
    #[test]
    fn an_import_stops_for_hand_work_and_merges_what_needs_none() {
        let root = scratch("import");
        let sh = git_shell(&root);
        let (source, zed_dir, fork) =
            (root.join("zed"), root.join("zed-checkout"), root.join("fork"));
        cmd!(sh, "git init -q -b main {source}").run().expect("init zed");
        let workspace =
            "[workspace.dependencies]\na = { path = \"crates/a\" }\nb = { path = \"crates/b\" }\n";
        write(&source, "Cargo.toml", workspace);
        write(&source, "crates/a/Cargo.toml", "[package]\nname = \"a\"\n");
        write(&source, "crates/a/src/lib.rs", "mod x;\n");
        write(&source, "crates/a/src/x.rs", "// v1\n");
        write(&source, "crates/a/run.sh", "echo\n");
        cmd!(sh, "chmod +x {source}/crates/a/run.sh").run().expect("chmod");
        std::os::unix::fs::symlink("../../LICENSE", source.join("crates/a/LICENSE")).expect("link");
        write(&source, "crates/zed/Cargo.toml", "[package]\nname = \"zed\"\n");
        let first = commit(&sh, &source, "zed one");
        cmd!(sh, "git clone -q {source} {zed_dir}").run().expect("clone zed");

        // The fork: an import of crates/a at `first`, then UPSTREAM, a manifest and a redirect.
        cmd!(sh, "git init -q -b main {fork}").run().expect("init fork");
        let archive = cmd!(sh, "git -C {source} archive --format=tar {first} crates/a")
            .output()
            .expect("archive");
        cmd!(sh, "tar -x -C {fork}").stdin(archive.stdout).run().expect("extract");
        let import = commit(&sh, &fork, "Extract");
        write(
            &fork,
            UPSTREAM,
            &format!(
                "zed_commit = \"{first}\"\nimport_commit = \"{import}\"\ntracked = [\n    \"crates/a\",\n]\n"
            ),
        );
        write(&fork, "Cargo.toml", "[workspace]\nmembers = [\"crates/a\"]\n");
        write(&fork, "crates/a/src/lib.rs", "#[path = \"fast/x.rs\"]\nmod x;\n");
        write(&fork, "crates/a/src/fast/x.rs", "// ours, from v1\n");
        commit(&sh, &fork, "gpui-fast");

        // zed changes the redirected file and makes a depend on a new crate b.
        write(&source, "crates/a/src/x.rs", "// v2\n");
        write(
            &source,
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\n[dependencies]\nb.workspace = true\n",
        );
        write(&source, "crates/b/Cargo.toml", "[package]\nname = \"b\"\n");
        write(&source, "crates/b/src/lib.rs", "// b\n");
        commit(&sh, &source, "zed two");
        write(&source, "crates/zed/src/main.rs", "fn main() {}\n");
        let third = commit(&sh, &source, "zed three, untracked");

        let zed = Import {
            tracking: Tracking {
                upstream: source.to_string(),
                upstream_branch: "main".to_owned(),
                base: first,
                base_date: "2026-09-01".to_owned(),
                checked: "2026-09-01".to_owned(),
                check_every_days: 7,
                paths: Vec::new(),
            },
            checkout: Utf8PathBuf::from("zed-checkout"),
        };
        let stopped = sync(&sh, &zed, &zed_dir, &fork, "main").expect_err("hand work");
        let report = format!("{stopped:#}");
        assert!(
            report.contains("crates/a/src/x.rs (+1 −1) → crates/a/src/fast/x.rs"),
            "port: {report}"
        );
        assert!(
            report.contains(
                "[workspace.dependencies] (the tracked crates now depend on them): crates/b"
            ),
            "wiring: {report}"
        );
        assert!(!report.contains("conflicts"), "no conflicts: {report}");

        let vendor = cmd!(sh, "git -C {fork} rev-parse MERGE_HEAD").read().expect("mid-merge");
        let parent = format!("{vendor}^");
        assert_eq!(
            cmd!(sh, "git -C {fork} rev-parse {parent}").read().ok().as_deref(),
            Some(import.as_str()),
            "on the last vendor commit"
        );
        let subject =
            cmd!(sh, "git -C {fork} log -1 --format=%s {vendor}").read().expect("subject");
        assert_eq!(
            subject,
            format!("zed: import {}", third.get(..8).expect("sha")),
            "vendor subject"
        );
        let ours = cmd!(sh, "git -C {fork} ls-tree -r {vendor} -- crates").read().expect("ours");
        let theirs = cmd!(sh, "git -C {source} ls-tree -r {third} -- crates/a crates/b")
            .read()
            .expect("theirs");
        assert_eq!(
            ours, theirs,
            "byte for byte, modes and the symlink included, zed's own crate left out"
        );
        let staged =
            Recorded::parse(&cmd!(sh, "git -C {fork} show :UPSTREAM").read().expect("staged"))
                .expect("parse");
        assert_eq!(
            staged,
            Recorded {
                zed_commit: third.clone(),
                import_commit: vendor.clone(),
                tracked: vec!["crates/a".to_owned(), "crates/b".to_owned()]
            },
            "UPSTREAM rewritten in the merge"
        );

        // Finish by hand, then a rerun has nothing to import.
        write(&fork, "Cargo.toml", "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n");
        write(&fork, "crates/a/src/fast/x.rs", "// ours, from v2\n");
        cmd!(sh, "git -C {fork} add -A").run().expect("add");
        cmd!(sh, "git -C {fork} commit -q --no-edit").run().expect("commit merge");
        let current = sync(&sh, &zed, &zed_dir, &fork, "main").expect("current");
        assert_eq!(current.sha, third, "current with zed's head");

        // A change nothing redirects merges on its own.
        write(&source, "crates/b/src/lib.rs", "// b2\n");
        let fourth = commit(&sh, &source, "zed four");
        let merged = sync(&sh, &zed, &zed_dir, &fork, "main").expect("merged");
        assert_eq!(merged.sha, fourth, "imported");
        let recorded = Recorded::at(&sh, &fork, "HEAD").expect("UPSTREAM");
        assert_eq!(recorded.zed_commit, fourth, "committed with the merge");
        let second_parent = cmd!(sh, "git -C {fork} rev-parse HEAD^2").read().expect("merge");
        assert_eq!(
            recorded.import_commit, second_parent,
            "the vendor commit is the merge's second parent"
        );
        let vendor_parent = format!("{second_parent}^");
        assert_eq!(
            cmd!(sh, "git -C {fork} rev-parse {vendor_parent}").read().ok(),
            Some(vendor),
            "vendor commits chain"
        );
        assert_eq!(
            std::fs::read_to_string(fork.join("crates/b/src/lib.rs")).ok().as_deref(),
            Some("// b2\n"),
            "in the tree"
        );
        std::fs::remove_dir_all(&root).expect("cleanup");
    }
}

//! What a check passed, staged as it passed it: `cargo xtask stage <name>…`.
//!
//! A lane's check snapshots the paths it names (`--only`) into a commit of their blobs. Staging
//! those paths from the working tree after a green check took whatever the lane had saved since,
//! and a later edit of a checked file (a call into a page not yet landed) turned the gate red
//! (0d8fc67d). So each check records the blobs it checked, with its verdict once known, and
//! `stage` puts exactly those into the index: never a check that failed, never one a later check
//! of the same lane superseded, never over a path main or the index moved since.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Context as _, Result, bail};
use camino::Utf8Path as Path;
use xshell::{Shell, cmd};

/// Where a check's record lives, beside its snapshot index: `target/remote/<name>/checked.json`.
const RECORD: &str = "checked.json";

/// `cargo xtask stage` options.
#[derive(clap::Args, Clone, Debug)]
pub struct StageOpts {
    /// The checks' names (each lane's `SLOPTY_LANE`): each one's paths go into the index with the
    /// blobs its last check passed.
    #[arg(required = true)]
    pub names: Vec<String>,
}

/// One path a check's snapshot changed against its parent, as it went into the snapshot.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
struct Entry {
    /// The file's mode, `000000` when the snapshot deleted it.
    mode: String,
    /// The blob (or a submodule's commit) the check held; all zeros when deleted.
    blob: String,
    path: String,
}

/// A check's record: what it snapshotted, and its verdict once it has one.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct Checked {
    /// The snapshot commit the check ran on.
    sha: String,
    /// Its parent: `HEAD` when the check began.
    parent: String,
    /// The `xtask` process that runs the check; only it records the verdict.
    pid: u32,
    /// When the check began, in milliseconds since the epoch.
    at: u128,
    /// `None` until the check ends; still `None` when it was stopped.
    passed: Option<bool>,
    /// The paths the check took (`--only`); none for a check of the whole tree or of `HEAD`.
    only: Vec<String>,
    entries: Vec<Entry>,
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// Records that the check in `dir` began on the snapshot `sha` taken of `only` (none: a whole
/// tree or `HEAD`, which `stage` refuses). It replaces the name's earlier record, so a check
/// still running, or stopped, is never staged by an older verdict.
pub fn started(sh: &Shell, dir: &Path, sha: &str, only: &[String]) {
    let entries = if only.is_empty() { Ok(Vec::new()) } else { entries(sh, sha) };
    let parent = format!("{sha}^");
    let parent = cmd!(sh, "git rev-parse {parent}").quiet().ignore_stderr().read();
    let record = match (entries, parent) {
        (Ok(entries), Ok(parent)) => Checked {
            sha: sha.to_owned(),
            parent,
            pid: std::process::id(),
            at: now_ms(),
            passed: None,
            only: only.to_vec(),
            entries,
        },
        // Unreadable: a record that stages nothing, so no older verdict stands for this check.
        _ => Checked {
            sha: sha.to_owned(),
            parent: String::new(),
            pid: std::process::id(),
            at: now_ms(),
            passed: None,
            only: Vec::new(),
            entries: Vec::new(),
        },
    };
    write(dir, &record);
}

/// Records the verdict of the check this process began in `dir`, if the record is still its own.
pub fn verdict(dir: &Path, passed: bool) {
    if let Some(mut record) = read(dir)
        && record.pid == std::process::id()
        && record.passed.is_none()
    {
        record.passed = Some(passed);
        write(dir, &record);
    }
}

fn write(dir: &Path, record: &Checked) {
    let path = dir.join(RECORD);
    let written = serde_json::to_vec_pretty(record)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| std::fs::write(&path, bytes).map_err(Into::into));
    if let Err(e) = written {
        println!("  what the check took is not recorded in {path}: {e}; `xtask stage` refuses it");
    }
}

fn read(dir: &Path) -> Option<Checked> {
    let text = std::fs::read_to_string(dir.join(RECORD)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The paths `sha` changed against its parent, with the mode and blob each holds there.
fn entries(sh: &Shell, sha: &str) -> Result<Vec<Entry>> {
    let raw = cmd!(sh, "git diff-tree -r -z --no-commit-id --root {sha}").quiet().read()?;
    let mut fields = raw.split('\0').filter(|f| !f.is_empty());
    let mut entries = Vec::new();
    while let Some(meta) = fields.next() {
        let path = fields.next().with_context(|| format!("no path after {meta}"))?;
        // `:<old mode> <new mode> <old blob> <new blob> <status>`
        let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [_, mode, _, blob, _status] = parts.as_slice() else {
            bail!("an unexpected diff-tree line: {meta}");
        };
        entries.push(Entry {
            mode: (*mode).to_owned(),
            blob: (*blob).to_owned(),
            path: path.to_owned(),
        });
    }
    Ok(entries)
}

/// The blob each of `paths` holds at `rev` (a commit, or the index with `None`), by path.
fn blobs(sh: &Shell, rev: Option<&str>, paths: &[&str]) -> Result<BTreeMap<String, String>> {
    if paths.is_empty() {
        return Ok(BTreeMap::new());
    }
    let listed = match rev {
        Some(rev) => cmd!(sh, "git ls-tree -r -z {rev} -- {paths...}").quiet().read()?,
        None => cmd!(sh, "git ls-files --stage -z -- {paths...}").quiet().read()?,
    };
    let mut blobs = BTreeMap::new();
    for line in listed.split('\0').filter(|l| !l.is_empty()) {
        // ls-tree: `<mode> <type> <blob>\t<path>`; ls-files: `<mode> <blob> <stage>\t<path>`.
        let Some((meta, path)) = line.split_once('\t') else { continue };
        let words: Vec<&str> = meta.split(' ').collect();
        let blob = if rev.is_some() { words.get(2) } else { words.get(1) };
        if let Some(blob) = blob {
            blobs.insert(path.to_owned(), (*blob).to_owned());
        }
    }
    Ok(blobs)
}

/// The records of every check under `remote` (`target/remote`), by name.
fn records(remote: &Path) -> BTreeMap<String, Checked> {
    let Ok(dirs) = remote.read_dir_utf8() else { return BTreeMap::new() };
    dirs.filter_map(Result::ok)
        .filter_map(|d| Some((d.file_name().to_owned(), read(d.path())?)))
        .collect()
}

/// Why `name`'s last check may not be staged, or what it stages.
fn stageable(
    sh: &Shell,
    name: &str,
    all: &BTreeMap<String, Checked>,
    head: &str,
) -> Result<(Vec<Entry>, String)> {
    let Some(record) = all.get(name) else {
        bail!("{name}: no check of that name is recorded here (`cargo xtask remote --only …`)");
    };
    let short = record.sha.get(..10).unwrap_or(&record.sha);
    match record.passed {
        None => bail!("{name}: its last check ({short}) has not ended, or was stopped"),
        Some(false) => bail!("{name}: its last check ({short}) failed"),
        Some(true) => {}
    }
    if record.entries.is_empty() {
        bail!(
            "{name}: its last check ({short}) took no `--only` paths, or checked `HEAD`: nothing \
             to stage by blob"
        );
    }
    let mine: Vec<&str> = record.entries.iter().map(|e| e.path.as_str()).collect();
    // A later check of the same lane over any of these paths: the lane edited them since.
    for (other, later) in all {
        if other != name
            && super::lane(other) == super::lane(name)
            && later.at > record.at
            && let Some(path) = later.entries.iter().find(|e| mine.contains(&e.path.as_str()))
        {
            bail!(
                "{name}: a later check of the lane, {other}, took {} too: stage that one, or check \
                 again",
                path.path
            );
        }
    }
    // Main moved one of these paths since the check: what it passed is not what would land.
    if record.parent != head {
        let then = blobs(sh, Some(&record.parent), &mine)?;
        let now = blobs(sh, Some(head), &mine)?;
        if let Some(path) = mine.iter().find(|p| then.get(**p) != now.get(**p)) {
            bail!("{name}: {path} changed on HEAD since its check ({short}): check again");
        }
    }
    // Something else staged over one of these paths: never overwritten.
    let staged = blobs(sh, None, &mine)?;
    let at_head = blobs(sh, Some(head), &mine)?;
    for entry in &record.entries {
        let path = entry.path.as_str();
        let index = staged.get(path);
        if index.is_some() && index != at_head.get(path) && index != Some(&entry.blob) {
            bail!("{name}: {path} is staged with other content: unstage it first");
        }
    }
    Ok((record.entries.clone(), short.to_owned()))
}

/// Stages each name's last green check as it passed, all or none; says what it staged and which
/// of those files the working tree holds otherwise now.
fn stage(sh: &Shell, root: &Path, names: &[String]) -> Result<Vec<String>> {
    let all = records(&root.join("target").join("remote"));
    let head = cmd!(sh, "git rev-parse HEAD").quiet().read()?;
    let mut taken: BTreeMap<String, (String, Entry)> = BTreeMap::new();
    let mut said = Vec::new();
    for name in names {
        let (entries, short) = stageable(sh, name, &all, &head)?;
        for entry in &entries {
            if let Some((other, held)) = taken.get(&entry.path)
                && held != entry
            {
                bail!("{name} and {other} checked {} with different content", entry.path);
            }
            taken.insert(entry.path.clone(), (name.clone(), entry.clone()));
        }
        said.push(format!(
            "staged {} path(s) of {name} as its check passed them ({short})",
            entries.len()
        ));
    }
    let mut info = String::new();
    for (_, entry) in taken.values() {
        // Mode 0 removes the path from the index.
        let (mode, blob) = if entry.mode.chars().all(|c| c == '0') {
            ("0", "0".repeat(40))
        } else {
            (entry.mode.as_str(), entry.blob.clone())
        };
        let _written = write!(info, "{mode} {blob}\t{}\0", entry.path);
    }
    cmd!(sh, "git update-index -z --index-info").stdin(info).quiet().run()?;
    let paths: Vec<&str> = taken.keys().map(String::as_str).collect();
    let edited = cmd!(sh, "git diff --name-only -- {paths...}").quiet().read()?;
    if !edited.trim().is_empty() {
        said.push(format!(
            "edited since their check, left unstaged: {}",
            edited.lines().collect::<Vec<_>>().join(" ")
        ));
    }
    Ok(said)
}

/// `cargo xtask stage <name>…`.
pub fn run(sh: &Shell, opts: &StageOpts) -> Result<()> {
    let root = crate::tools::repo_root()?;
    for line in stage(sh, &root, &opts.names)? {
        println!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use camino::{Utf8Path, Utf8PathBuf};
    use xshell::{Shell, cmd};

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

    fn write(root: &Utf8Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, text).expect("write");
    }

    /// A check of `paths` under `name`, as `remote` takes it: snapshot, record, verdict.
    fn check(sh: &Shell, root: &Utf8Path, name: &str, paths: &[&str], passed: Option<bool>) {
        let dir = root.join("target").join("remote").join(name);
        std::fs::create_dir_all(&dir).expect("dir");
        let only: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
        let sha = super::super::snapshot(sh, root, &dir, name, &only).expect("snapshot");
        super::started(sh, &dir, &sha, &only);
        if let Some(passed) = passed {
            super::verdict(&dir, passed);
        }
    }

    fn staged(sh: &Shell, path: &str) -> String {
        let spec = format!(":{path}");
        cmd!(sh, "git show {spec}").read().unwrap_or_default()
    }

    /// The index takes the blobs a green check passed, not what the lane saved after it; a red
    /// check, an unfinished one, one a later check of the lane superseded, and one whose path
    /// main moved since are refused, and refusing stages nothing.
    #[test]
    fn staging_takes_what_the_check_passed_and_refuses_the_rest() {
        let dir = std::env::temp_dir().join(format!("xtask-stage-{}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&dir);
        let root = Utf8PathBuf::from_path_buf(dir).expect("utf8");
        let sh = git_shell(&root);
        write(&root, ".gitignore", "/target\n");
        write(&root, "a/one.rs", "one\n");
        write(&root, "a/gone.rs", "gone\n");
        write(&root, "b/two.rs", "two\n");
        cmd!(sh, "git init -q").run().expect("init");
        cmd!(sh, "git add -A").run().expect("add");
        cmd!(sh, "git commit -q -m first").run().expect("commit");

        write(&root, "a/one.rs", "checked\n");
        std::fs::remove_file(root.join("a/gone.rs")).expect("rm");
        write(&root, "a/new.rs", "new\n");
        check(&sh, &root, "L", &["a"], Some(true));
        // Saved after the check: a call into something not landed.
        write(&root, "a/one.rs", "edited after\n");
        let said = super::stage(&sh, &root, &["L".to_owned()]).expect("stages");
        assert_eq!(staged(&sh, "a/one.rs"), "checked", "the checked blob, not the tree's");
        assert_eq!(staged(&sh, "a/new.rs"), "new");
        assert_eq!(staged(&sh, "a/gone.rs"), "", "a deletion goes too");
        assert!(said.iter().any(|s| s.contains("left unstaged: a/one.rs")), "{said:?}");
        cmd!(sh, "git reset -q").run().expect("reset");

        // A red check, and one not ended, stage nothing.
        check(&sh, &root, "R", &["b"], Some(false));
        write(&root, "b/two.rs", "two, red\n");
        check(&sh, &root, "R", &["b"], Some(false));
        let red = super::stage(&sh, &root, &["L".to_owned(), "R".to_owned()]);
        assert!(red.expect_err("red").to_string().contains("failed"));
        assert_eq!(staged(&sh, "a/one.rs"), "one", "all or none: L was not staged either");
        check(&sh, &root, "R", &["b"], None);
        let running = super::stage(&sh, &root, &["R".to_owned()]);
        assert!(running.expect_err("unended").to_string().contains("not ended"));

        // A later check of the lane under another name over the same path supersedes L's.
        check(&sh, &root, "L-x", &["a/one.rs"], Some(false));
        let superseded = super::stage(&sh, &root, &["L".to_owned()]);
        assert!(superseded.expect_err("later").to_string().contains("L-x"));
        std::fs::remove_dir_all(root.join("target/remote/L-x")).expect("rm");

        // Main moved a checked path since the check.
        write(&root, "a/one.rs", "main's\n");
        cmd!(sh, "git commit -q -am main").run().expect("main moves");
        let moved = super::stage(&sh, &root, &["L".to_owned()]);
        assert!(moved.expect_err("moved").to_string().contains("changed on HEAD"));
        let _gone = std::fs::remove_dir_all(&root);
    }
}

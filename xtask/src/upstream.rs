//! `xtask upstream`: keep the GPUI and gpui-kit forks current with their upstreams.
//!
//! The forks (`aislopware/zed` for `gpui`, `gpui_ios`, `gpui_platform`; `aislopware/gpui-kit`)
//! are a handful of commits on top of a moving upstream. `xtask/upstream.toml` records where
//! each fork branch sits (`base`, the upstream commit it was last rebased onto, and its date).
//! `check` fetches both upstreams and reports the drift; `sync` rebases each fork onto the
//! newest upstream, build-checks it, pushes it, moves this workspace's `Cargo.lock` pins and
//! rewrites the base lines. The gate prints a warning when a base is older than
//! [`STALE_AFTER_DAYS`] days.
//!
//! The checkouts live under the main checkout of this repository (shared by every worktree),
//! cloned with `--filter=blob:none` on first use.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Subcommand;
use serde::Deserialize;
use xshell::{Shell, cmd};

use crate::tools::{repo_root, step};

/// The configuration file, relative to the repository root.
const CONFIG: &str = "xtask/upstream.toml";
/// A fork base older than this many days earns a gate warning.
const STALE_AFTER_DAYS: i64 = 7;
/// Sync order: gpui-kit's lock resolves against the zed fork, so zed goes first.
const ORDER: [&str; 2] = ["zed", "gpui-kit"];
/// How many tags newer than the base `check` lists per fork.
const TAGS_SHOWN: usize = 6;

/// `xtask upstream` subcommands.
#[derive(Subcommand, Debug)]
pub enum UpstreamCmd {
    /// Fetch both upstreams and print how far each fork base is behind, plus the pin dates.
    Check,
    /// Rebase each fork onto its upstream, build-check, push, and move the workspace pins.
    Sync {
        /// Only this fork (`zed` or `gpui-kit`).
        #[arg(long)]
        only: Option<String>,
        /// Rebase and build-check but neither push nor move the pins.
        #[arg(long)]
        no_push: bool,
    },
}

/// One fork, as written in `upstream.toml`.
#[derive(Debug, Deserialize)]
struct Fork {
    /// The upstream repository.
    upstream: String,
    /// The upstream branch we follow.
    upstream_branch: String,
    /// Our fork.
    #[serde(rename = "fork")]
    url: String,
    /// The fork branch the workspace pins (`Cargo.toml` `branch = …`).
    #[serde(rename = "fork_branch")]
    branch: String,
    /// Checkout directory, relative to the main checkout of this repository.
    checkout: Utf8PathBuf,
    /// The branch in that checkout carrying our commits.
    local_branch: String,
    /// The upstream commit the fork branch was last rebased onto.
    base: String,
    /// Its commit date (`YYYY-MM-DD`).
    base_date: String,
}

/// The whole file, in [`ORDER`].
#[derive(Debug, Deserialize)]
struct Config {
    zed: Fork,
    #[serde(rename = "gpui-kit")]
    gpui_kit: Fork,
}

impl Config {
    fn load(root: &Utf8Path) -> Result<Self> {
        let path = root.join(CONFIG);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
        toml::from_str(&text).with_context(|| format!("parsing {path}"))
    }

    const fn forks(&self) -> [(&'static str, &Fork); 2] {
        [(ORDER[0], &self.zed), (ORDER[1], &self.gpui_kit)]
    }
}

pub fn run(sh: &Shell, cmd: &UpstreamCmd) -> Result<()> {
    let root = repo_root()?;
    let config = Config::load(&root)?;
    let main = main_checkout(sh)?;
    match cmd {
        UpstreamCmd::Check => {
            for (name, fork) in config.forks() {
                check(sh, &root, &main, name, fork)?;
            }
            Ok(())
        }
        UpstreamCmd::Sync { only, no_push } => {
            let mut synced = Vec::new();
            for (name, fork) in config.forks() {
                if only.as_deref().is_some_and(|o| o != name) {
                    continue;
                }
                synced.push((name, sync(sh, &main, name, fork, *no_push)?));
            }
            ensure!(!synced.is_empty(), "no fork named {only:?} in {CONFIG}");
            if *no_push {
                println!("✔ rebased and checked; nothing pushed (--no-push)");
                return Ok(());
            }
            let _dir = sh.push_dir(&root);
            step("cargo update", &cmd!(sh, "cargo update -p gpui -p gpui-kit"))?;
            for (name, base) in &synced {
                write_base(&root, name, base)?;
            }
            println!(
                "✔ forks pushed and pins moved; now `cargo xtask gate`, then `cargo xtask e2e app` \
                 and `cargo xtask e2e ios --sim iphone`, and record the bases in docs/DECISIONS.md"
            );
            Ok(())
        }
    }
}

/// Print a warning line when a fork base is older than [`STALE_AFTER_DAYS`]. Never fails: the
/// gate calls it and a broken config is reported as the warning itself.
pub fn warn_if_stale() {
    let report = || -> Result<Vec<String>> {
        let root = repo_root()?;
        let config = Config::load(&root)?;
        let today = today_days()?;
        let mut stale = Vec::new();
        for (name, fork) in config.forks() {
            let age = today.saturating_sub(days_from_civil(&fork.base_date)?);
            if age > STALE_AFTER_DAYS {
                stale.push(format!("{name} base is {age} days old ({})", fork.base_date));
            }
        }
        Ok(stale)
    };
    match report() {
        Ok(stale) => {
            for line in stale {
                println!("⚠ upstream: {line}; run `cargo xtask upstream check`");
            }
        }
        Err(error) => println!("⚠ upstream: {CONFIG} unreadable ({error:#})"),
    }
}

/// A fetched upstream: head commit and date.
#[derive(Debug)]
struct Head {
    sha: String,
    date: String,
}

fn check(sh: &Shell, root: &Utf8Path, main: &Utf8Path, name: &str, fork: &Fork) -> Result<()> {
    let dir = ensure_checkout(sh, main, fork)?;
    let _dir = sh.push_dir(&dir);
    let upstream = fetch(sh, &fork.upstream, &fork.upstream_branch)?;
    let fork_head = fetch(sh, &fork.url, &fork.branch)?;
    let range = format!("{}..{}", fork.base, upstream.sha);
    let behind = cmd!(sh, "git rev-list --count {range}").read()?;
    let tags = tags_since(sh, &fork.base, &upstream.sha)?;
    let age = today_days()?.saturating_sub(days_from_civil(&fork.base_date)?);
    println!(
        "{name}: base {} ({}, {age} days old); upstream {} at {} ({}): {behind} commits ahead",
        short(&fork.base),
        fork.base_date,
        fork.upstream_branch,
        short(&upstream.sha),
        upstream.date,
    );
    if !tags.is_empty() {
        println!("  tags since base: {}", tags.join(", "));
    }
    let pin = lock_pin(root, fork)?;
    let pin_date = commit_date(sh, &pin).unwrap_or_else(|_| "not fetched".to_owned());
    let state = if pin == fork_head.sha { "= fork head" } else { "≠ fork head" };
    println!(
        "  Cargo.lock pins {} ({pin_date}) {state} {} ({})",
        short(&pin),
        short(&fork_head.sha),
        fork_head.date
    );
    let branch = &fork.local_branch;
    let local = cmd!(sh, "git rev-parse --verify --quiet {branch}")
        .ignore_stderr()
        .read()
        .unwrap_or_default();
    if local != fork_head.sha {
        println!("  note: local branch {} is at {}", fork.local_branch, short(&local));
    }
    Ok(())
}

/// Rebase, build-check and push one fork. Returns the new base (upstream head sha + date).
fn sync(sh: &Shell, main: &Utf8Path, name: &str, fork: &Fork, no_push: bool) -> Result<Head> {
    let dir = ensure_checkout(sh, main, fork)?;
    let _dir = sh.push_dir(&dir);
    println!("▶ {name}: {dir}");
    // `REBASE_HEAD` outlives a finished rebase; the state directories do not.
    let mid_rebase = ["rebase-merge", "rebase-apply"].into_iter().any(|state| {
        cmd!(sh, "git rev-parse --git-path {state}")
            .quiet()
            .read()
            .is_ok_and(|path| Utf8Path::new(path.trim()).exists())
    });
    ensure!(!mid_rebase, "{dir} is mid-rebase; finish or abort it first");
    let dirty = cmd!(sh, "git status --porcelain").read()?;
    ensure!(dirty.is_empty(), "{dir} has uncommitted changes:\n{dirty}");

    let upstream = fetch(sh, &fork.upstream, &fork.upstream_branch)?;
    let fork_head = fetch(sh, &fork.url, &fork.branch)?;
    let branch = &fork.local_branch;
    let fork_sha = &fork_head.sha;
    let local = cmd!(sh, "git rev-parse --verify --quiet {branch}").ignore_stderr().read().ok();
    match local {
        None => {
            step("branch", &cmd!(sh, "git branch --no-track {branch} {fork_sha}"))?;
        }
        Some(local) if local == fork_head.sha => {}
        Some(local) => {
            let fork_is_ancestor =
                cmd!(sh, "git merge-base --is-ancestor {fork_sha} {local}").run().is_ok();
            ensure!(
                fork_is_ancestor,
                "{}'s {} ({}) has diverged from {} ({}); reset it or push it first",
                dir,
                fork.local_branch,
                short(&local),
                fork.url,
                short(&fork_head.sha)
            );
            println!("  note: {} is ahead of the fork; rebasing it too", fork.local_branch);
        }
    }

    let onto = &upstream.sha;
    let already = cmd!(sh, "git merge-base --is-ancestor {onto} {branch}").run().is_ok();
    if already {
        println!("  {name} already contains upstream {}", short(onto));
    } else if cmd!(sh, "git rebase {onto} {branch}").run().is_err() {
        resolve_conflicts(sh, &dir, onto)?;
    }

    for (title, args) in build_checks(name) {
        let args = args.split(' ');
        step(title, &cmd!(sh, "cargo {args...}"))?;
    }

    if !no_push {
        let url = &fork.url;
        let refspec = format!("{}:{}", fork.local_branch, fork.branch);
        let lease = format!("--force-with-lease={}:{}", fork.branch, fork_head.sha);
        step("push", &cmd!(sh, "git push {url} {refspec} {lease}"))?;
    }
    Ok(upstream)
}

/// The per-fork `cargo` invocations run inside the checkout after a rebase.
fn build_checks(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        "zed" => &[
            (
                "check gpui + gpui_ios (ios-sim)",
                "check -p gpui -p gpui_ios --target aarch64-apple-ios-sim",
            ),
            ("check gpui + gpui_platform (host)", "check -p gpui -p gpui_platform"),
        ],
        "gpui-kit" => &[("check gpui-kit", "check -p gpui-kit --features component,assets")],
        _ => &[],
    }
}

/// A rebase stopped on conflicts. `Cargo.lock` alone is mechanical (our commits only add git
/// sources to it): take upstream's lock and let cargo re-add ours. Anything else is left
/// mid-rebase for a person or an agent, with the file list in the error.
fn resolve_conflicts(sh: &Shell, dir: &Utf8Path, upstream: &str) -> Result<()> {
    loop {
        let conflicted = cmd!(sh, "git diff --name-only --diff-filter=U").read()?;
        let files: Vec<&str> = conflicted.lines().collect();
        if files != ["Cargo.lock"] {
            bail!(
                "rebase stopped in {dir} on conflicts in: {}\n  resolve them there (read the \
                 upstream change before choosing a side), `git rebase --continue`, then run \
                 `cargo xtask upstream sync` again",
                if files.is_empty() {
                    "(none listed; see `git status`)".to_owned()
                } else {
                    files.join(", ")
                }
            );
        }
        let lock = cmd!(sh, "git show {upstream}:Cargo.lock").read()?;
        sh.write_file(dir.join("Cargo.lock"), lock)?;
        step("cargo update -w (lock from upstream)", &cmd!(sh, "cargo update -w"))?;
        cmd!(sh, "git add Cargo.lock").run()?;
        let _editor = sh.push_env("GIT_EDITOR", "true");
        if cmd!(sh, "git rebase --continue").run().is_ok() {
            return Ok(());
        }
    }
}

/// Clone the checkout on first use (fork branch, blobless), returning its absolute path.
fn ensure_checkout(sh: &Shell, main: &Utf8Path, fork: &Fork) -> Result<Utf8PathBuf> {
    let dir = main.join(&fork.checkout);
    if !dir.join(".git").exists() {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
        }
        let (url, remote_branch, branch) = (&fork.url, &fork.branch, &fork.local_branch);
        step(
            &format!("clone {url}"),
            &cmd!(sh, "git clone --filter=blob:none --branch {remote_branch} {url} {dir}"),
        )?;
        let _dir = sh.push_dir(&dir);
        cmd!(sh, "git branch --no-track {branch} {remote_branch}").run()?;
        cmd!(sh, "git switch --quiet {branch}").run()?;
    }
    Ok(dir)
}

/// Fetch `branch` from `url` through the remote that carries it (added when missing, so the
/// clone's blob filter applies), returning the fetched head.
fn fetch(sh: &Shell, url: &str, branch: &str) -> Result<Head> {
    let remote = remote_for(sh, url)?;
    let filter: &[&str] = if is_partial(sh) { &["--filter=blob:none"] } else { &[] };
    cmd!(sh, "git fetch --quiet --no-tags {filter...} {remote} {branch}")
        .quiet()
        .run()
        .with_context(|| format!("fetching {branch} from {url}"))?;
    let sha = cmd!(sh, "git rev-parse FETCH_HEAD").read()?;
    let date = commit_date(sh, &sha)?;
    // Tags are what `check` reports; `--no-tags` above keeps the branch fetch small and the
    // forced refspec lets moving tags (zed's `nightly`) update. Losing them only costs a line.
    if cmd!(sh, "git fetch --quiet {filter...} {remote} +refs/tags/*:refs/tags/*")
        .quiet()
        .run()
        .is_err()
    {
        println!("  note: tags from {url} not fetched");
    }
    Ok(Head { sha, date })
}

/// The remote whose URL is `url`, added as `upstream`/`fork`-style names when absent.
fn remote_for(sh: &Shell, url: &str) -> Result<String> {
    let names = cmd!(sh, "git remote").read()?;
    for name in names.lines() {
        let existing = cmd!(sh, "git remote get-url {name}").read()?;
        if same_repo(&existing, url) {
            return Ok(name.to_owned());
        }
    }
    let name = url
        .trim_end_matches(".git")
        .rsplit('/')
        .nth(1)
        .map_or_else(|| "remote".to_owned(), str::to_owned);
    cmd!(sh, "git remote add {name} {url}").run()?;
    Ok(name)
}

fn same_repo(a: &str, b: &str) -> bool {
    a.trim_end_matches('/').trim_end_matches(".git")
        == b.trim_end_matches('/').trim_end_matches(".git")
}

fn is_partial(sh: &Shell) -> bool {
    cmd!(sh, "git config --get extensions.partialclone").ignore_stderr().quiet().read().is_ok()
        || cmd!(sh, "git config --get remote.origin.partialclonefilter")
            .ignore_stderr()
            .quiet()
            .read()
            .is_ok()
}

fn commit_date(sh: &Shell, sha: &str) -> Result<String> {
    cmd!(sh, "git log -1 --format=%cs {sha}").ignore_stderr().read().map_err(Into::into)
}

/// Tags reachable from `head` that contain `base` but are not `base` itself, newest first.
fn tags_since(sh: &Shell, base: &str, head: &str) -> Result<Vec<String>> {
    let out = cmd!(sh, "git tag --contains {base} --merged {head} --sort=-creatordate").read()?;
    let mut tags = Vec::new();
    for tag in out.lines() {
        let peeled = format!("{tag}^{{commit}}");
        let at = cmd!(sh, "git rev-parse {peeled}").read()?;
        if at != base {
            tags.push(tag.to_owned());
        }
        if tags.len() == TAGS_SHOWN {
            break;
        }
    }
    Ok(tags)
}

/// The commit this workspace's `Cargo.lock` pins for the fork's git source.
fn lock_pin(root: &Utf8Path, fork: &Fork) -> Result<String> {
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).context("reading Cargo.lock")?;
    let needle = format!("source = \"git+{}?branch={}#", fork.url, fork.branch);
    lock.lines()
        .find_map(|line| line.strip_prefix(&needle))
        .map(|rest| rest.trim_end_matches('"').to_owned())
        .with_context(|| format!("Cargo.lock has no entry for {} branch {}", fork.url, fork.branch))
}

/// Rewrite the `base` and `base_date` lines of one `[section]` in place, keeping comments.
fn write_base(root: &Utf8Path, name: &str, head: &Head) -> Result<()> {
    use std::fmt::Write as _;

    let path = root.join(CONFIG);
    let text = std::fs::read_to_string(&path)?;
    let header = format!("[{name}]");
    let mut inside = false;
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if line.starts_with('[') {
            inside = line.trim() == header;
        }
        if inside && line.starts_with("base = ") {
            let _written = write!(out, "base = \"{}\"", head.sha);
        } else if inside && line.starts_with("base_date = ") {
            let _written = write!(out, "base_date = \"{}\"", head.date);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    std::fs::write(&path, out).with_context(|| format!("writing {path}"))
}

/// The main checkout of this repository: the parent of the shared git directory, which is the
/// same for every worktree.
fn main_checkout(sh: &Shell) -> Result<Utf8PathBuf> {
    let git_dir = cmd!(sh, "git rev-parse --path-format=absolute --git-common-dir").read()?;
    Utf8PathBuf::from(git_dir)
        .parent()
        .map(Utf8Path::to_path_buf)
        .context("git common dir has no parent")
}

fn short(sha: &str) -> &str {
    sha.get(..10).unwrap_or(sha)
}

/// Days since the Unix epoch, today (UTC).
fn today_days() -> Result<i64> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    i64::try_from(secs.checked_div(86_400).unwrap_or(0)).map_err(Into::into)
}

/// Days since the Unix epoch for a `YYYY-MM-DD` date (Howard Hinnant's `days_from_civil`).
#[expect(
    clippy::arithmetic_side_effects,
    reason = "the civil-to-days formula cannot overflow an i64 for a four-digit year"
)]
fn days_from_civil(date: &str) -> Result<i64> {
    let mut parts = date.splitn(3, '-').map(str::parse::<i64>);
    let (Some(Ok(y)), Some(Ok(m)), Some(Ok(d))) = (parts.next(), parts.next(), parts.next()) else {
        bail!("bad date {date:?}; expected YYYY-MM-DD");
    };
    ensure!((1..=12).contains(&m) && (1..=31).contains(&d), "bad date {date:?}");
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Ok(era * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_dates() {
        assert_eq!(days_from_civil("1970-01-01").ok(), Some(0), "epoch");
        assert_eq!(days_from_civil("2000-03-01").ok(), Some(11_017), "leap century");
        assert_eq!(days_from_civil("2026-09-05").ok(), Some(20_701), "today when written");
        assert!(days_from_civil("2026-13-01").is_err(), "month out of range");
        assert!(days_from_civil("nope").is_err(), "garbage");
    }

    #[test]
    fn base_lines_are_rewritten_in_their_section_only() {
        let dir = std::env::temp_dir().join(format!("xtask-upstream-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("xtask")).expect("tmp dir");
        let root = Utf8PathBuf::from_path_buf(dir.clone()).expect("utf8 tmp dir");
        std::fs::write(
            root.join(CONFIG),
            "# note\n[zed]\nbase = \"a\"\nbase_date = \"2020-01-01\"\n\n[gpui-kit]\nbase = \"b\"\nbase_date = \"2020-01-02\"\n",
        )
        .expect("write");
        write_base(&root, "gpui-kit", &Head { sha: "c".to_owned(), date: "2026-09-05".to_owned() })
            .expect("rewrite");
        let text = std::fs::read_to_string(root.join(CONFIG)).expect("read");
        std::fs::remove_dir_all(&dir).expect("cleanup");
        assert_eq!(
            text,
            "# note\n[zed]\nbase = \"a\"\nbase_date = \"2020-01-01\"\n\n[gpui-kit]\nbase = \"c\"\nbase_date = \"2026-09-05\"\n",
            "only the gpui-kit section changes"
        );
    }

    #[test]
    fn lock_pin_reads_the_git_source() {
        let dir = std::env::temp_dir().join(format!("xtask-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let root = Utf8PathBuf::from_path_buf(dir.clone()).expect("utf8 tmp dir");
        std::fs::write(
            root.join("Cargo.lock"),
            "[[package]]\nname = \"gpui\"\nsource = \"git+https://x/zed.git?branch=slopty#abc\"\n",
        )
        .expect("write");
        let fork = Fork {
            upstream: String::new(),
            upstream_branch: String::new(),
            url: "https://x/zed.git".to_owned(),
            branch: "slopty".to_owned(),
            checkout: Utf8PathBuf::new(),
            local_branch: String::new(),
            base: String::new(),
            base_date: String::new(),
        };
        let pin = lock_pin(&root, &fork);
        std::fs::remove_dir_all(&dir).expect("cleanup");
        assert_eq!(pin.ok().as_deref(), Some("abc"), "sha after the fragment");
    }
}

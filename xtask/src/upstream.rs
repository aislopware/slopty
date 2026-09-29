//! `xtask upstream`: keep the forks Slopty builds on current with their upstreams.
//!
//! Four forks carry our commits on their default branch: `aislopware/gpui-fast` (`gpui`,
//! `gpui_ios`, `gpui_platform`) on longbridge/gpui-fast, `aislopware/gpui-kit`,
//! `aislopware/ghostty` (checked out as `vendor/ghostty`) and `aislopware/libghostty-rs`, which
//! pins a ghostty commit. gpui-fast is GPUI imported flat out of zed, and longbridge takes a
//! newer zed only now and then, so the fork imports zed itself ([`zed`]) right after taking
//! longbridge's branch: whatever longbridge already imported is never imported twice.
//!
//! `xtask/upstream.toml` records where each source stands (`base`, the upstream commit last
//! taken, its date, and `checked`, the day a sync last confirmed it current). `check` fetches the
//! upstreams and reports the drift; `sync` rebases or merges each fork onto its upstream, imports
//! zed into gpui-fast, re-pins libghostty-rs to the ghostty fork, build-checks, pushes, moves
//! `vendor/ghostty` and this workspace's `Cargo.lock` pins and rewrites the base lines ([`plan`]
//! orders it). Once a source was last known current longer ago than its own `check_every_days`,
//! the gate asks its upstream for the branch head (`git ls-remote`, no fetch) and warns when it
//! moved past the base; for a source with `paths`, only when the move touched one of them.
//!
//! The checkouts live under the main checkout of this repository (shared by every worktree),
//! cloned with `--filter=blob:none` on first use.

mod ghostty;
mod watch;
mod zed;

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Subcommand;
use serde::Deserialize;
use xshell::{Shell, cmd};

use crate::tools::{repo_root, step};

/// The configuration file, relative to the repository root.
const CONFIG: &str = "xtask/upstream.toml";
/// The fork zed is imported into.
const GPUI_FAST: &str = "gpui-fast";
/// The terminal's source, `vendor/ghostty`.
const GHOSTTY: &str = "ghostty";
/// The binding, which pins a ghostty commit.
const LIBGHOSTTY_RS: &str = "libghostty-rs";
/// Sync order: gpui-kit's lock resolves against the gpui-fast fork, so gpui-fast goes first;
/// libghostty-rs pins the ghostty fork's head, so ghostty goes before it.
const ORDER: [&str; 4] = [GPUI_FAST, "gpui-kit", GHOSTTY, LIBGHOSTTY_RS];
/// How many tags newer than the base `check` lists per source.
const TAGS_SHOWN: usize = 6;
/// GitHub's compare lists at most this many changed files; a list that long may be cut short.
const COMPARE_FILES: usize = 300;

/// `xtask upstream` subcommands.
#[derive(Subcommand, Debug)]
pub enum UpstreamCmd {
    /// Fetch the upstreams and zed and print how far each fork is behind, plus the pins.
    Check,
    /// Take each upstream into its fork, import zed into gpui-fast, build-check, push, and move
    /// the workspace pins.
    Sync {
        /// Only this fork (`gpui-fast`, `gpui-kit`, `ghostty` or `libghostty-rs`; `zed` is
        /// `gpui-fast`, which takes longbridge's branch before it imports zed, and `ghostty`
        /// takes libghostty-rs with it, which pins it).
        #[arg(long)]
        only: Option<String>,
        /// Take the upstreams and build-check but neither push nor move the pins.
        #[arg(long)]
        no_push: bool,
    },
    /// Print a line for each change upstream as it happens: a head that moved, a pull request
    /// opened, updated, merged or closed. Watches the forks' upstreams, noq and objc2; never syncs.
    Watch {
        /// Seconds between looks.
        #[arg(long, default_value_t = 300)]
        interval: u64,
        /// Look once, print, and exit.
        #[arg(long)]
        once: bool,
    },
}

/// Where a source stands on its upstream: what the gate's staleness check reads.
#[derive(Debug, Deserialize)]
struct Tracking {
    /// The upstream repository.
    upstream: String,
    /// The upstream branch we follow.
    upstream_branch: String,
    /// The upstream commit last taken.
    base: String,
    /// Its commit date (`YYYY-MM-DD`).
    base_date: String,
    /// The day `sync` last confirmed the source current (`YYYY-MM-DD`); a quiet upstream keeps
    /// an old `base_date`, and this is what keeps the gate from calling it stale.
    checked: String,
    /// How many days the source may go unconfirmed before the gate asks its upstream again: none
    /// for an upstream that lands several changes a day (every gate asks), a week for the others.
    check_every_days: i64,
    /// The upstream paths that reach us (a directory ends in `/`). When given, a head move that
    /// touches none of them is no news to the gate or `watch`, and `check` lists the commits
    /// that do touch them. None: every move counts.
    #[serde(default)]
    paths: Vec<String>,
}

impl Tracking {
    /// Days since the source was last known current: the later of the base commit and the last
    /// confirming `sync`.
    fn days_since_current(&self, today: i64) -> Result<i64> {
        let base = days_from_civil(&self.base_date)?;
        let checked = days_from_civil(&self.checked)?;
        Ok(today.saturating_sub(base.max(checked)).max(0))
    }

    /// Whether the source's interval has run out, so the gate asks its upstream.
    fn due(&self, today: i64) -> Result<bool> {
        Ok(self.days_since_current(today)? >= self.check_every_days)
    }
}

/// How a fork takes its upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Strategy {
    /// Replay our commits onto the upstream head (force-pushed with a lease).
    Rebase,
    /// Merge the upstream head into our branch. gpui-fast's zed imports are merges of vendor
    /// commits that sit beside its branch; a rebase would replay or drop them.
    Merge,
}

/// One fork, as written in `upstream.toml`.
#[derive(Debug, Deserialize)]
struct Fork {
    #[serde(flatten)]
    tracking: Tracking,
    /// Our fork.
    #[serde(rename = "fork")]
    url: String,
    /// The branch carrying our commits, in the fork and in the checkout alike: the fork's default
    /// branch, which `Cargo.toml` pins by leaving `branch` out.
    branch: String,
    /// Checkout directory, relative to the main checkout of this repository.
    checkout: Utf8PathBuf,
    strategy: Strategy,
}

/// zed, which is not forked but imported into gpui-fast.
#[derive(Debug, Deserialize)]
struct Import {
    /// `base` is the zed commit the fork is known current with: the one it imported, or a later
    /// one that changed nothing in the tracked directories.
    #[serde(flatten)]
    tracking: Tracking,
    /// A zed clone, relative to the main checkout; only its objects are used.
    checkout: Utf8PathBuf,
}

/// A source vendored into this repository, not forked: `upstream watch` follows it.
#[derive(Debug, Deserialize)]
struct Vendored {
    upstream: String,
    upstream_branch: String,
    /// The upstream commit the vendored copy was taken from.
    base: String,
}

/// The whole file.
#[derive(Debug, Deserialize)]
struct Config {
    #[serde(rename = "gpui-fast")]
    gpui_fast: Fork,
    zed: Import,
    #[serde(rename = "gpui-kit")]
    gpui_kit: Fork,
    ghostty: Fork,
    #[serde(rename = "libghostty-rs")]
    libghostty_rs: Fork,
    noq: Vendored,
    objc2: Vendored,
}

impl Config {
    fn load(root: &Utf8Path) -> Result<Self> {
        let path = root.join(CONFIG);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
        toml::from_str(&text).with_context(|| format!("parsing {path}"))
    }

    /// The forks, in [`ORDER`].
    const fn forks(&self) -> [(&'static str, &Fork); 4] {
        [
            (ORDER[0], &self.gpui_fast),
            (ORDER[1], &self.gpui_kit),
            (ORDER[2], &self.ghostty),
            (ORDER[3], &self.libghostty_rs),
        ]
    }

    fn fork(&self, name: &str) -> Result<&Fork> {
        self.forks()
            .into_iter()
            .find_map(|(fork, config)| (fork == name).then_some(config))
            .with_context(|| format!("no fork named {name:?}"))
    }

    /// Everything the gate watches.
    const fn sources(&self) -> [(&'static str, &Tracking); 5] {
        [
            (ORDER[0], &self.gpui_fast.tracking),
            ("zed", &self.zed.tracking),
            (ORDER[1], &self.gpui_kit.tracking),
            (ORDER[2], &self.ghostty.tracking),
            (ORDER[3], &self.libghostty_rs.tracking),
        ]
    }
}

/// The forks `--only` picks, in sync order. zed goes through gpui-fast, which takes longbridge's
/// branch first; ghostty brings libghostty-rs, whose pin must follow it.
fn selected(only: Option<&str>) -> Result<Vec<&'static str>> {
    match only {
        None => Ok(ORDER.to_vec()),
        Some("zed") => Ok(vec![GPUI_FAST]),
        Some(GHOSTTY) => Ok(vec![GHOSTTY, LIBGHOSTTY_RS]),
        Some(name) => match ORDER.iter().find(|fork| **fork == name) {
            Some(fork) => Ok(vec![*fork]),
            None => bail!("no fork named {name:?} in {CONFIG}; expected zed or one of {ORDER:?}"),
        },
    }
}

/// One step of a sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Take the upstream into the fork's local branch and build-check it; for libghostty-rs,
    /// also pin the ghostty head and regenerate the bindings. Nothing leaves the machine.
    Take(&'static str),
    /// Push the fork's branch and confirm the remote holds it.
    Push(&'static str),
    /// Move `vendor/ghostty` to the ghostty fork's pushed head.
    MoveSubmodule,
    /// `cargo update` the pushed forks' packages and confirm `Cargo.lock` pins their heads.
    MovePins,
}

/// The steps of a sync of `names`, in order. Every fork is taken and checked before it is
/// pushed. ghostty is the exception to pushing right after its take: its check is the binding's
/// build against it, so it is published only after libghostty-rs was taken, and then in the
/// order the pins demand: the ghostty fork, `vendor/ghostty`, the binding that pins it, the
/// lock that pins the binding. A failure anywhere before leaves every published pin as it was.
fn plan(names: &[&'static str], no_push: bool) -> Result<Vec<Step>> {
    let has = |name: &str| names.contains(&name);
    ensure!(!has(GHOSTTY) || has(LIBGHOSTTY_RS), "ghostty syncs with libghostty-rs, which pins it");
    let mut steps = Vec::new();
    for name in ORDER.into_iter().filter(|name| has(name)) {
        steps.push(Step::Take(name));
        if no_push || name == GHOSTTY {
            continue;
        }
        if name == LIBGHOSTTY_RS && has(GHOSTTY) {
            steps.extend([Step::Push(GHOSTTY), Step::MoveSubmodule]);
        }
        steps.push(Step::Push(name));
    }
    if !no_push {
        steps.push(Step::MovePins);
    }
    Ok(steps)
}

/// The workspace package that pins each fork in `Cargo.lock` (`cargo update -p`); ghostty is
/// pinned through libghostty-rs and the submodule, not the lock.
fn lock_package(fork: &str) -> Option<&'static str> {
    match fork {
        GPUI_FAST => Some("gpui"),
        "gpui-kit" => Some("gpui-kit"),
        LIBGHOSTTY_RS => Some("libghostty-vt"),
        _ => None,
    }
}

/// Whether `file` is `path`, or under it when `path` ends in `/`.
fn under(file: &str, path: &str) -> bool {
    if path.ends_with('/') { file.starts_with(path) } else { file == path }
}

/// The files that fall under `paths`.
fn touching<'a>(files: &'a [String], paths: &[String]) -> Vec<&'a str> {
    files
        .iter()
        .map(String::as_str)
        .filter(|file| paths.iter().any(|path| under(file, path)))
        .collect()
}

/// Whether a change to `files` can reach us: always without `paths`, and when the list may have
/// been cut short at `cap` entries, since the files past it are unknown.
fn relevant(files: &[String], paths: &[String], cap: usize) -> bool {
    paths.is_empty() || files.len() >= cap || !touching(files, paths).is_empty()
}

/// What GitHub says lies between two commits of an upstream.
#[derive(Debug, Deserialize)]
struct Compare {
    ahead: u64,
    files: Vec<String>,
}

/// Ask GitHub (`gh api`) what lies between `from` and `to` on the upstream at `url`. The file
/// list stops at [`COMPARE_FILES`].
fn compare(sh: &Shell, url: &str, from: &str, to: &str) -> Result<Compare> {
    let endpoint = format!("repos/{}/compare/{from}...{to}?per_page=1", repo_slug(url));
    let jq = "{ahead: .ahead_by, files: [.files[]?.filename]}";
    let out = cmd!(sh, "gh api {endpoint} --jq {jq}").quiet().ignore_stderr().read()?;
    serde_json::from_str(&out).with_context(|| format!("reading gh api {endpoint}"))
}

pub fn run(sh: &Shell, cmd: &UpstreamCmd) -> Result<()> {
    let root = repo_root()?;
    let config = Config::load(&root)?;
    let main = main_checkout(sh)?;
    match cmd {
        UpstreamCmd::Check => {
            for (name, fork) in config.forks() {
                let checked = check(sh, &root, &main, name, fork, &config.ghostty)?;
                if name == GPUI_FAST {
                    let zed_dir = ensure_checkout(
                        sh,
                        &main,
                        &config.zed.checkout,
                        &config.zed.tracking.upstream,
                        None,
                    )?;
                    zed::check(
                        sh,
                        &config.zed,
                        &zed_dir,
                        &checked.dir,
                        &checked.fork_head,
                        &checked.upstream,
                    )?;
                }
            }
            Ok(())
        }
        UpstreamCmd::Watch { interval, once } => {
            watch::run(sh, &root, std::time::Duration::from_secs(*interval), *once)
        }
        UpstreamCmd::Sync { only, no_push } => {
            sync_all(sh, &root, &main, &config, only.as_deref(), *no_push)
        }
    }
}

/// Run a sync's [`plan`].
fn sync_all(
    sh: &Shell,
    root: &Utf8Path,
    main: &Utf8Path,
    config: &Config,
    only: Option<&str>,
    no_push: bool,
) -> Result<()> {
    let names = selected(only)?;
    let mut taken: Vec<(&'static str, Taken)> = Vec::new();
    let mut moved_submodule = false;
    for next in plan(&names, no_push)? {
        match next {
            Step::Take(name) => {
                let fork = config.fork(name)?;
                let dir = if name == GHOSTTY {
                    ghostty::prepare(sh, main, fork)?
                } else {
                    ensure_checkout(sh, main, &fork.checkout, &fork.url, Some(&fork.branch))?
                };
                let pin = if name == LIBGHOSTTY_RS {
                    Some(ghostty_source(sh, main, &config.ghostty, &taken)?)
                } else {
                    None
                };
                let zed = if name == GPUI_FAST {
                    let zed = &config.zed;
                    let url = &zed.tracking.upstream;
                    Some((zed, ensure_checkout(sh, main, &zed.checkout, url, None)?))
                } else {
                    None
                };
                let done = take(sh, dir, name, fork, zed, pin.as_ref())?;
                taken.push((name, done));
            }
            Step::Push(name) => push(sh, config.fork(name)?, taken_of(&taken, name)?)?,
            Step::MoveSubmodule => {
                let head = &taken_of(&taken, GHOSTTY)?.head;
                ghostty::move_submodule(sh, main, &config.ghostty, head)?;
                moved_submodule = true;
            }
            Step::MovePins => move_pins(sh, root, config, &taken)?,
        }
    }
    if no_push {
        println!("✔ upstreams taken and checked; nothing pushed (--no-push)");
        return Ok(());
    }
    let today = civil_from_days(today_days()?);
    for (name, done) in &taken {
        write_base(root, name, &done.upstream, &today)?;
        if let Some(zed) = &done.zed {
            write_base(root, "zed", zed, &today)?;
        }
    }
    if moved_submodule {
        let checkout = &config.ghostty.checkout;
        let recorded = ghostty::recorded(sh, root, checkout)?;
        let head = &taken_of(&taken, GHOSTTY)?.head;
        if recorded != *head {
            println!(
                "  {checkout} is at {} and this repository records {}: stage it with `git add \
                 {checkout}` beside Cargo.lock",
                short(head),
                short(&recorded)
            );
        }
    }
    println!(
        "✔ forks pushed and pins moved; now `cargo xtask gate`, then `cargo xtask e2e app` and \
         `cargo xtask e2e ios --sim iphone`, and record the bases in docs/decisions/tooling.md"
    );
    Ok(())
}

/// The ghostty source libghostty-rs is pinned to: the ghostty fork's branch as this sync took it,
/// or else `vendor/ghostty` as it stands, which must then be on the fork already.
fn ghostty_source(
    sh: &Shell,
    main: &Utf8Path,
    fork: &Fork,
    taken: &[(&str, Taken)],
) -> Result<GhosttySource> {
    if let Ok(done) = taken_of(taken, GHOSTTY) {
        return Ok(GhosttySource {
            tree: done.dir.clone(),
            head: done.head.clone(),
            fork: fork.url.clone(),
        });
    }
    let tree = main.join(&fork.checkout);
    let head = {
        let _dir = sh.push_dir(&tree);
        cmd!(sh, "git rev-parse HEAD").read()?
    };
    ghostty::ensure_on_fork(sh, &tree, fork, &head)?;
    Ok(GhosttySource { tree, head, fork: fork.url.clone() })
}

/// What this sync took for `name`.
fn taken_of<'a>(taken: &'a [(&str, Taken)], name: &str) -> Result<&'a Taken> {
    taken
        .iter()
        .find_map(|(fork, done)| (*fork == name).then_some(done))
        .with_context(|| format!("{name} was not taken by this sync"))
}

/// `cargo update` the packages of the forks this sync pushed, then confirm the lock pins each
/// fork's pushed head.
fn move_pins(sh: &Shell, root: &Utf8Path, config: &Config, taken: &[(&str, Taken)]) -> Result<()> {
    let pinned: Vec<(&str, &Taken)> = taken
        .iter()
        .filter_map(|(name, done)| lock_package(name).map(|package| (package, done)))
        .collect();
    if pinned.is_empty() {
        return Ok(());
    }
    let packages: Vec<&str> = pinned.iter().flat_map(|(package, _)| ["-p", package]).collect();
    let _dir = sh.push_dir(root);
    step("cargo update", &cmd!(sh, "cargo update {packages...}"))?;
    for (name, done) in taken {
        if lock_package(name).is_none() {
            continue;
        }
        let url = &config.fork(name)?.url;
        let pin = lock_pin(root, url)?;
        ensure!(
            pin.as_deref() == Some(done.head.as_str()),
            "Cargo.lock pins {} for {name}, not the pushed head {}",
            pin.as_deref().map_or("nothing", short),
            short(&done.head)
        );
    }
    Ok(())
}

/// Print a warning line for each source past its `check_every_days` whose upstream branch moved
/// past its base; an upstream that cannot be reached is warned about by the dates alone. Never
/// fails: the gate calls it and a broken config is reported as the warning itself.
pub fn warn_if_stale() {
    let report = || -> Result<Vec<String>> {
        let root = repo_root()?;
        let config = Config::load(&root)?;
        let today = today_days()?;
        let sh = Shell::new()?;
        let mut stale = Vec::new();
        for (name, source) in config.sources() {
            if !source.due(today)? {
                continue;
            }
            let age = source.days_since_current(today)?;
            match remote_head(&sh, source) {
                Ok(head) if head == source.base => {}
                Ok(head) => {
                    // Without an answer from GitHub the move counts: it cannot be ruled out.
                    let between = (!source.paths.is_empty())
                        .then(|| compare(&sh, &source.upstream, &source.base, &head).ok())
                        .flatten();
                    let what = match &between {
                        Some(c) if !relevant(&c.files, &source.paths, COMPARE_FILES) => continue,
                        Some(c) => format!(
                            " ({} commits; {} changed files under its paths)",
                            c.ahead,
                            touching(&c.files, &source.paths).len()
                        ),
                        None => String::new(),
                    };
                    stale.push(format!(
                        "{name}'s upstream {} moved to {} since base {} ({}){what}; run `cargo \
                         xtask upstream sync --only {name}`",
                        source.upstream_branch,
                        short(&head),
                        short(&source.base),
                        source.base_date,
                    ));
                }
                Err(error) => stale.push(format!(
                    "{name} was last known current {age} days ago (base {}, checked {}; upstream \
                     unreachable: {error:#}); run `cargo xtask upstream check`",
                    source.base_date, source.checked
                )),
            }
        }
        Ok(stale)
    };
    match report() {
        Ok(stale) => {
            for line in stale {
                println!("⚠ upstream: {line}");
            }
        }
        Err(error) => println!("⚠ upstream: {CONFIG} unreadable ({error:#})"),
    }
}

/// The upstream branch's head, asked of the remote without fetching. A stalled connection gives
/// up after a few seconds rather than holding the gate.
fn remote_head(sh: &Shell, source: &Tracking) -> Result<String> {
    let (url, branch) = (&source.upstream, format!("refs/heads/{}", source.upstream_branch));
    let out =
        cmd!(sh, "git -c http.lowSpeedLimit=1000 -c http.lowSpeedTime=5 ls-remote {url} {branch}")
            .quiet()
            .ignore_stderr()
            .read()?;
    out.split_whitespace().next().map(str::to_owned).context("the branch is not on the remote")
}

/// A fetched head: commit and date.
#[derive(Debug)]
struct Head {
    sha: String,
    date: String,
}

/// What `check` fetched for a fork, for the zed check that follows gpui-fast's.
#[derive(Debug)]
struct Checked {
    dir: Utf8PathBuf,
    fork_head: String,
    upstream: String,
}

fn check(
    sh: &Shell,
    root: &Utf8Path,
    main: &Utf8Path,
    name: &str,
    fork: &Fork,
    ghostty: &Fork,
) -> Result<Checked> {
    let dir = ensure_checkout(sh, main, &fork.checkout, &fork.url, Some(&fork.branch))?;
    let _dir = sh.push_dir(&dir);
    let tracking = &fork.tracking;
    let upstream = fetch(sh, &tracking.upstream, &tracking.upstream_branch)?;
    let fork_head = fetch(sh, &fork.url, &fork.branch)?;
    let range = format!("{}..{}", tracking.base, upstream.sha);
    let behind = cmd!(sh, "git rev-list --count {range}").read()?;
    let tags = tags_since(sh, &tracking.base, &upstream.sha)?;
    let age = tracking.days_since_current(today_days()?)?;
    println!(
        "{name}: base {} ({}, current {age} days ago); upstream {} at {} ({}): {behind} commits \
         ahead",
        short(&tracking.base),
        tracking.base_date,
        tracking.upstream_branch,
        short(&upstream.sha),
        upstream.date,
    );
    if !tags.is_empty() {
        println!("  tags since base: {}", tags.join(", "));
    }
    if name == GHOSTTY {
        ghostty::report(sh, root, fork, &fork_head.sha, &upstream.sha)?;
    } else {
        lock_report(sh, root, fork, &fork_head)?;
    }
    let branch = &fork.branch;
    let local = cmd!(sh, "git rev-parse --verify --quiet {branch}")
        .ignore_stderr()
        .read()
        .unwrap_or_default();
    if local != fork_head.sha {
        println!("  note: local branch {branch} is at {}", short(&local));
    }
    if name == LIBGHOSTTY_RS {
        ghostty_pin_report(sh, main, ghostty, &fork_head.sha)?;
    }
    Ok(Checked { dir, fork_head: fork_head.sha, upstream: upstream.sha })
}

/// The commit this workspace's `Cargo.lock` pins for a fork, against the fork's head.
fn lock_report(sh: &Shell, root: &Utf8Path, fork: &Fork, fork_head: &Head) -> Result<()> {
    match lock_pin(root, &fork.url)? {
        Some(pin) => {
            let pin_date = commit_date(sh, &pin).unwrap_or_else(|_| "not fetched".to_owned());
            let state = if pin == fork_head.sha { "= fork head" } else { "≠ fork head" };
            println!(
                "  Cargo.lock pins {} ({pin_date}) {state} {} ({})",
                short(&pin),
                short(&fork_head.sha),
                fork_head.date
            );
        }
        None => println!(
            "  Cargo.lock pins nothing from {}; fork head {} ({})",
            fork.url,
            short(&fork_head.sha),
            fork_head.date
        ),
    }
    Ok(())
}

/// The binding pins one ghostty commit (`GHOSTTY_COMMIT` in its sys build script) and the
/// workspace builds from `vendor/ghostty`; the two must agree.
fn ghostty_pin_report(sh: &Shell, main: &Utf8Path, ghostty: &Fork, fork_head: &str) -> Result<()> {
    let build_rs = format!("{fork_head}:crates/libghostty-vt-sys/build.rs");
    let script = cmd!(sh, "git show {build_rs}").read()?;
    let pinned = ghostty::pinned(&script)?;
    let _dir = sh.push_dir(main.join(&ghostty.checkout));
    let head = cmd!(sh, "git rev-parse HEAD").read()?;
    let state = if head == pinned { "=" } else { "≠" };
    println!(
        "  binding pins ghostty {} {state} {} {}",
        short(pinned),
        ghostty.checkout,
        short(&head)
    );
    Ok(())
}

/// The ghostty source libghostty-rs is pinned to and build-checked against.
#[derive(Debug)]
struct GhosttySource {
    /// A work tree holding `head`, handed to the build as `GHOSTTY_SOURCE_DIR`.
    tree: Utf8PathBuf,
    head: String,
    /// The ghostty fork, which the pin must name.
    fork: String,
}

/// What one fork's take left: the upstream head it took (and for gpui-fast the zed head it is now
/// current with), and the local branch to push.
#[derive(Debug)]
struct Taken {
    dir: Utf8PathBuf,
    upstream: Head,
    zed: Option<Head>,
    /// The fork's head before the take, which the push leases against.
    fork_head: String,
    /// The local branch's head after the take: what the push publishes.
    head: String,
}

/// Take the upstream into one fork's local branch in `dir`, import zed (from its clone) when
/// given, pin ghostty when given, and build-check. Nothing leaves the machine.
fn take(
    sh: &Shell,
    dir: Utf8PathBuf,
    name: &str,
    fork: &Fork,
    zed: Option<(&Import, Utf8PathBuf)>,
    ghostty: Option<&GhosttySource>,
) -> Result<Taken> {
    let _dir = sh.push_dir(&dir);
    println!("▶ {name}: {dir}");
    ensure_idle(sh, &dir)?;

    let upstream = fetch(sh, &fork.tracking.upstream, &fork.tracking.upstream_branch)?;
    let fork_head = fetch(sh, &fork.url, &fork.branch)?;
    reconcile_local(sh, &dir, fork, &fork_head, &upstream)?;

    let (branch, onto) = (&fork.branch, &upstream.sha);
    cmd!(sh, "git switch --quiet {branch}").run()?;
    let already = cmd!(sh, "git merge-base --is-ancestor {onto} {branch}").run().is_ok();
    if already {
        println!("  {name} already contains upstream {}", short(onto));
    } else {
        let taken = match fork.strategy {
            Strategy::Rebase => cmd!(sh, "git rebase --quiet {onto}").run(),
            Strategy::Merge => {
                let message = format!(
                    "Merge {} {} at {}",
                    repo_slug(&fork.tracking.upstream),
                    fork.tracking.upstream_branch,
                    short(onto)
                );
                cmd!(sh, "git merge --no-edit -m {message} {onto}").run()
            }
        };
        if taken.is_err() {
            resolve_conflicts(sh, &dir, name, onto, fork.strategy)?;
        }
    }

    let zed = match zed {
        Some((zed, zed_dir)) => Some(zed::sync(sh, zed, &zed_dir, &dir, branch)?),
        None => None,
    };

    let _source = match ghostty {
        Some(source) => {
            ghostty::repin(sh, &dir, &source.fork, &source.tree, &source.head)?;
            Some(sh.push_env("GHOSTTY_SOURCE_DIR", &source.tree))
        }
        None => None,
    };
    refresh_lock(sh)?;
    for (title, command) in build_checks(name) {
        let mut words = command.split(' ');
        let program = words.next().unwrap_or_default();
        step(title, &cmd!(sh, "{program} {words...}"))?;
    }
    let head = cmd!(sh, "git rev-parse {branch}").read()?;
    Ok(Taken { dir, upstream, zed, fork_head: fork_head.sha, head })
}

/// Push a taken fork's branch, leased on the head the take started from, and confirm the remote
/// now holds what was taken.
fn push(sh: &Shell, fork: &Fork, taken: &Taken) -> Result<()> {
    let _dir = sh.push_dir(&taken.dir);
    let branch = &fork.branch;
    let remote = remote_for(sh, &fork.url)?;
    let refspec = format!("{}:refs/heads/{branch}", taken.head);
    let lease = format!("--force-with-lease={branch}:{}", taken.fork_head);
    step(
        &format!("push {} to {}", short(&taken.head), repo_slug(&fork.url)),
        &cmd!(sh, "git push --quiet {remote} {refspec} {lease}"),
    )?;
    let url = &fork.url;
    let at = cmd!(sh, "git ls-remote {url} refs/heads/{branch}").read()?;
    let at = at.split_whitespace().next().unwrap_or_default();
    ensure!(
        at == taken.head,
        "{url} {branch} is at {} after the push, not {}",
        short(at),
        short(&taken.head)
    );
    Ok(())
}

/// A sync starts from a checkout with no rebase or merge in progress and nothing uncommitted.
fn ensure_idle(sh: &Shell, dir: &Utf8Path) -> Result<()> {
    // `REBASE_HEAD` outlives a finished rebase; the state directories do not.
    let mid_rebase = ["rebase-merge", "rebase-apply"].into_iter().any(|state| {
        cmd!(sh, "git rev-parse --git-path {state}")
            .quiet()
            .read()
            .is_ok_and(|path| Utf8Path::new(path.trim()).exists())
    });
    ensure!(!mid_rebase, "{dir} is mid-rebase; finish or abort it first");
    let mid_merge =
        cmd!(sh, "git rev-parse -q --verify MERGE_HEAD").quiet().ignore_stdout().run().is_ok();
    ensure!(!mid_merge, "{dir} is mid-merge; finish it (`git commit`) or abort it first");
    let dirty = cmd!(sh, "git status --porcelain").read()?;
    ensure!(dirty.is_empty(), "{dir} has uncommitted changes:\n{dirty}");
    Ok(())
}

/// Make the local branch the one to build on: created at the fork head when missing, kept when it
/// is the fork head or ahead of it, refused when it has diverged.
fn reconcile_local(
    sh: &Shell,
    dir: &Utf8Path,
    fork: &Fork,
    fork_head: &Head,
    upstream: &Head,
) -> Result<()> {
    let branch = &fork.branch;
    let fork_sha = &fork_head.sha;
    let local = cmd!(sh, "git rev-parse --verify --quiet {branch}").ignore_stderr().read().ok();
    let Some(local) = local else {
        return step("branch", &cmd!(sh, "git branch --no-track {branch} {fork_sha}"));
    };
    if local == fork_head.sha {
        return Ok(());
    }
    // The fork head is usually an ancestor. Under a rebase it is not when the previous `sync`
    // rebased this branch and then stopped (a build-check failed and the fix landed here), so fall
    // back to patches: a branch that carries every fork patch is ahead, not diverged. `git
    // cherry` marks a patch missing from the branch with `+`.
    let fork_is_ancestor =
        cmd!(sh, "git merge-base --is-ancestor {fork_sha} {local}").run().is_ok();
    let carries_every_patch = || {
        cmd!(sh, "git cherry {local} {fork_sha}")
            .quiet()
            .read()
            .is_ok_and(|out| !out.lines().any(|line| line.starts_with('+')))
    };
    // A conflict resolved by hand rewrites the patch, so `git cherry` cannot vouch for it either:
    // then accept the branch when it sits on a newer upstream than the fork and replays the
    // fork's patches by subject, in order, before any fixes of its own. Upstream may have moved
    // again since, so the branch's own base is what counts.
    let upstream_sha = &upstream.sha;
    let replays_every_patch = || {
        let read = |cmd: xshell::Cmd<'_>| cmd.quiet().read().ok();
        let subjects = |range: &str| {
            read(cmd!(sh, "git log --format=%s --reverse {range}"))
                .map(|out| out.lines().map(str::to_owned).collect::<Vec<_>>())
        };
        let (Some(fork_base), Some(local_base)) = (
            read(cmd!(sh, "git merge-base {fork_sha} {upstream_sha}")),
            read(cmd!(sh, "git merge-base {local} {upstream_sha}")),
        ) else {
            return false;
        };
        let (fork_base, local_base) = (fork_base.trim(), local_base.trim());
        let rebased =
            cmd!(sh, "git merge-base --is-ancestor {fork_base} {local_base}").quiet().run().is_ok();
        let (Some(replayed), Some(patches)) = (
            subjects(&format!("{local_base}..{local}")),
            subjects(&format!("{fork_base}..{fork_sha}")),
        ) else {
            return false;
        };
        rebased && replayed.starts_with(&patches)
    };
    let rebased_ahead =
        fork.strategy == Strategy::Rebase && (carries_every_patch() || replays_every_patch());
    ensure!(
        fork_is_ancestor || rebased_ahead,
        "{}'s {} ({}) has diverged from {} ({}); reset it or push it first",
        dir,
        fork.branch,
        short(&local),
        fork.url,
        short(&fork_head.sha)
    );
    println!("  note: {branch} is ahead of the fork; taking the upstream into it too");
    Ok(())
}

/// A merge that git finished on its own can still leave `Cargo.lock` behind the merged manifests
/// (upstream adds a dependency, our side's lock never saw it). Bring the lock up to the manifests
/// without upgrading anything, and commit it when it moved, so the pushed head builds `--locked`.
fn refresh_lock(sh: &Shell) -> Result<()> {
    if cmd!(sh, "git ls-files --error-unmatch Cargo.lock").quiet().ignore_stdout().run().is_err() {
        return Ok(());
    }
    step("cargo update -w", &cmd!(sh, "cargo update -w"))?;
    if cmd!(sh, "git diff --quiet -- Cargo.lock").quiet().run().is_err() {
        cmd!(sh, "git add Cargo.lock").run()?;
        let subject = "chore: bring Cargo.lock up to the merged manifests";
        step("commit the refreshed Cargo.lock", &cmd!(sh, "git commit --quiet -m {subject}"))?;
    }
    Ok(())
}

/// The per-fork checks run inside the checkout once it holds the upstream: a program and its
/// arguments, space-separated. Cargo runs `--locked`: the lock [`refresh_lock`] left is the one
/// pushed, and a check must not rewrite it behind the push.
fn build_checks(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        GPUI_FAST => &[
            // gpui-fast's own rule that upstream files hold only small hooks.
            ("check-upstream", "script/check-upstream"),
            (
                "check gpui + gpui_ios (ios-sim)",
                "cargo check --locked -p gpui -p gpui_ios --target aarch64-apple-ios-sim",
            ),
            ("check gpui + gpui_platform (host)", "cargo check --locked -p gpui -p gpui_platform"),
        ],
        // The whole workspace, story and examples included: a sync that built only the kit once
        // pushed a head whose shell and story did not compile. `too_many_arguments` is allowed
        // because upstream's own functions trip it.
        "gpui-kit" => &[
            (
                "clippy gpui-kit workspace",
                "cargo clippy --workspace --all-targets --locked -- --deny warnings -A \
                 clippy::too_many_arguments",
            ),
            (
                "test gpui-kit story + recipes",
                "cargo test --locked -p gpui-component-story -p gpui-kit-recipes --features \
                 gpui-component-story/test-support",
            ),
            // The shell hosts draw real windows, so they are what sees a change in when
            // gpui-fast re-renders a view.
            (
                "test gpui-kit shells",
                "cargo test --locked --no-fail-fast -p gpui-shell -p gpui-component-shell",
            ),
        ],
        // `take` points `GHOSTTY_SOURCE_DIR` at the ghostty tree it pins, so these build the
        // ghostty the pin names, and they are ghostty's check too: the rebased terminal must
        // build under the binding and pass the binding's tests before either is pushed.
        LIBGHOSTTY_RS => &[
            ("check libghostty-vt", "cargo check --locked -p libghostty-vt --all-targets"),
            ("test libghostty-vt", "cargo test --locked -p libghostty-vt"),
        ],
        _ => &[],
    }
}

/// A rebase or merge stopped on conflicts. `Cargo.lock` alone is mechanical (our commits only add
/// git sources to it): take upstream's lock and let cargo re-add ours. Anything else is left
/// mid-way for a person or an agent, with the file list in the error.
fn resolve_conflicts(
    sh: &Shell,
    dir: &Utf8Path,
    name: &str,
    upstream: &str,
    strategy: Strategy,
) -> Result<()> {
    let (what, finish) = match strategy {
        Strategy::Rebase => ("rebase", "`git rebase --continue`"),
        Strategy::Merge => ("merge", "`git commit`"),
    };
    loop {
        let conflicted = cmd!(sh, "git diff --name-only --diff-filter=U").read()?;
        let files: Vec<&str> = conflicted.lines().collect();
        if files != ["Cargo.lock"] {
            bail!(
                "{what} stopped in {dir} on conflicts in: {}\n  resolve them there (read the \
                 upstream change before choosing a side), {finish}, then run `cargo xtask \
                 upstream sync --only {name}` again; nothing was pushed and no pin moved",
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
        let finished = match strategy {
            Strategy::Rebase => cmd!(sh, "git rebase --continue").run(),
            Strategy::Merge => cmd!(sh, "git commit --no-edit").run(),
        };
        if finished.is_ok() {
            return Ok(());
        }
    }
}

/// Clone the checkout on first use (blobless; at `branch`, or without a work tree when there is
/// none to build on), returning its absolute path.
fn ensure_checkout(
    sh: &Shell,
    main: &Utf8Path,
    checkout: &Utf8Path,
    url: &str,
    branch: Option<&str>,
) -> Result<Utf8PathBuf> {
    let dir = main.join(checkout);
    if !dir.join(".git").exists() {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
        }
        let at: Vec<&str> = branch.map_or_else(|| vec!["--no-checkout"], |b| vec!["--branch", b]);
        step(
            &format!("clone {url}"),
            &cmd!(sh, "git clone --filter=blob:none {at...} {url} {dir}"),
        )?;
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

/// The remote whose URL names the same repository as `url`, added under the owner's name when
/// absent.
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

/// Whether two remote URLs name one repository, over HTTPS or SSH, with or without `.git`.
fn same_repo(a: &str, b: &str) -> bool {
    repo_key(a) == repo_key(b)
}

/// `host/owner/repo` for a remote URL (`https://host/owner/repo.git`, `git@host:owner/repo`,
/// `ssh://git@host/owner/repo`); a local path stays as it is.
fn repo_key(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url).trim_end_matches('/');
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest.to_owned(),
        None if url.starts_with('/') || url.starts_with('.') => return url.to_owned(),
        None => url.replacen(':', "/", 1),
    };
    let rest = match rest.split_once('@') {
        Some((user, host)) if !user.contains('/') => host,
        _ => &rest,
    };
    rest.to_ascii_lowercase()
}

/// `owner/repo` for a remote URL, for merge messages.
fn repo_slug(url: &str) -> String {
    let key = repo_key(url);
    let mut parts = key.rsplit('/');
    match (parts.next(), parts.next()) {
        (Some(repo), Some(owner)) => format!("{owner}/{repo}"),
        _ => key.clone(),
    }
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

/// The commit this workspace's `Cargo.lock` pins for the git source at `url`, if it has one.
fn lock_pin(root: &Utf8Path, url: &str) -> Result<Option<String>> {
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).context("reading Cargo.lock")?;
    Ok(lock_pin_in(&lock, url))
}

fn lock_pin_in(lock: &str, url: &str) -> Option<String> {
    lock.lines().find_map(|line| {
        let source = line.strip_prefix("source = \"git+")?.strip_suffix('"')?;
        let (location, sha) = source.rsplit_once('#')?;
        let location = location.split_once('?').map_or(location, |(repo, _)| repo);
        same_repo(location, url).then(|| sha.to_owned())
    })
}

/// Rewrite the `base`, `base_date` and `checked` lines of one `[section]` in place, keeping
/// comments; `checked` becomes `today`.
fn write_base(root: &Utf8Path, name: &str, head: &Head, today: &str) -> Result<()> {
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
        } else if inside && line.starts_with("checked = ") {
            let _written = write!(out, "checked = \"{today}\"");
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

/// `YYYY-MM-DD` for a count of days since the Unix epoch (Howard Hinnant's `civil_from_days`).
#[expect(
    clippy::arithmetic_side_effects,
    reason = "the days-to-civil formula cannot overflow an i64 for any day count from the epoch"
)]
fn civil_from_days(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
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

    fn tracking(base_date: &str, checked: &str, check_every_days: i64) -> Tracking {
        Tracking {
            upstream: String::new(),
            upstream_branch: String::new(),
            base: String::new(),
            base_date: base_date.to_owned(),
            checked: checked.to_owned(),
            check_every_days,
            paths: Vec::new(),
        }
    }

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
        let before = "# note\n[gpui-fast]\nbase = \"a\"\nbase_date = \"2020-01-01\"\nchecked = \"2020-01-01\"\n\n\
                      [zed]\nbase = \"b\"\nbase_date = \"2020-01-02\"\nchecked = \"2020-01-03\"\n";
        std::fs::write(root.join(CONFIG), before).expect("write");
        let head = Head { sha: "c".to_owned(), date: "2026-09-05".to_owned() };
        write_base(&root, "zed", &head, "2026-09-12").expect("rewrite");
        let text = std::fs::read_to_string(root.join(CONFIG)).expect("read");
        std::fs::remove_dir_all(&dir).expect("cleanup");
        assert_eq!(
            text,
            "# note\n[gpui-fast]\nbase = \"a\"\nbase_date = \"2020-01-01\"\nchecked = \"2020-01-01\"\n\n\
             [zed]\nbase = \"c\"\nbase_date = \"2026-09-05\"\nchecked = \"2026-09-12\"\n",
            "only the zed section changes"
        );
    }

    #[test]
    fn civil_dates_round_trip() {
        for date in ["1970-01-01", "2000-02-29", "2026-09-12", "2100-12-31"] {
            let days = days_from_civil(date).expect("valid date");
            assert_eq!(civil_from_days(days), date, "round trip");
        }
    }

    #[test]
    fn a_quiet_upstream_is_current_from_its_last_check() {
        let today = days_from_civil("2026-09-12").expect("date");
        let source = tracking("2026-09-01", "2026-09-12", 7);
        assert_eq!(source.days_since_current(today).ok(), Some(0), "checked today");
        let source = tracking("2026-09-01", "2026-08-01", 7);
        assert_eq!(source.days_since_current(today).ok(), Some(11), "the later date wins");
    }

    #[test]
    fn a_source_is_due_past_its_own_interval() {
        let today = days_from_civil("2026-09-13").expect("date");
        assert_eq!(
            tracking("2026-09-10", "2026-09-11", 7).due(today).ok(),
            Some(false),
            "two days into a week"
        );
        assert_eq!(
            tracking("2026-09-10", "2026-09-11", 2).due(today).ok(),
            Some(true),
            "two days, and two was the interval"
        );
        assert_eq!(
            tracking("2026-09-10", "2026-09-13", 0).due(today).ok(),
            Some(true),
            "no interval: asked even the day of a sync"
        );
    }

    #[test]
    fn the_checked_in_config_parses_and_the_gate_watches_zed() {
        let root = repo_root().expect("root");
        let config = Config::load(&root).expect("xtask/upstream.toml");
        assert_eq!(config.gpui_fast.strategy, Strategy::Merge, "gpui-fast merges its upstream");
        assert_eq!(config.gpui_kit.strategy, Strategy::Rebase, "gpui-kit rebases");
        assert_eq!(config.gpui_fast.tracking.check_every_days, 0, "every gate asks longbridge");
        let names: Vec<&str> = config.sources().iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            [GPUI_FAST, "zed", "gpui-kit", GHOSTTY, LIBGHOSTTY_RS],
            "what the gate asks"
        );
        let ghostty = &config.ghostty;
        assert_eq!(ghostty.strategy, Strategy::Rebase, "our ghostty commits replay onto main");
        assert_eq!(ghostty.checkout, "vendor/ghostty", "the submodule is the checkout");
        assert!(ghostty.tracking.paths.iter().any(|p| p == "src/terminal/"), "filtered");
        assert!(config.gpui_kit.tracking.paths.is_empty(), "every gpui-kit move counts");
        let pin = lock_pin(&root, &config.libghostty_rs.url).expect("Cargo.lock");
        assert!(pin.is_some(), "Cargo.lock pins the binding's fork");
    }

    #[test]
    fn only_picks_forks_and_zed_means_gpui_fast() {
        assert_eq!(selected(None).ok(), Some(ORDER.to_vec()), "all, gpui-fast first");
        assert_eq!(selected(Some("zed")).ok(), Some(vec![GPUI_FAST]), "zed through gpui-fast");
        assert_eq!(selected(Some("gpui-kit")).ok(), Some(vec!["gpui-kit"]), "one fork");
        assert_eq!(
            selected(Some(GHOSTTY)).ok(),
            Some(vec![GHOSTTY, LIBGHOSTTY_RS]),
            "ghostty brings the binding that pins it"
        );
        assert_eq!(selected(Some(LIBGHOSTTY_RS)).ok(), Some(vec![LIBGHOSTTY_RS]), "the binding");
        assert!(selected(Some("zed-fork")).is_err(), "unknown");
    }

    #[test]
    fn a_sync_checks_everything_before_it_publishes_a_pin() {
        use Step::{MovePins, MoveSubmodule, Push, Take};
        let all = plan(&ORDER, false).expect("plan");
        assert_eq!(
            all,
            [
                Take(GPUI_FAST),
                Push(GPUI_FAST),
                Take("gpui-kit"),
                Push("gpui-kit"),
                Take(GHOSTTY),
                Take(LIBGHOSTTY_RS),
                Push(GHOSTTY),
                MoveSubmodule,
                Push(LIBGHOSTTY_RS),
                MovePins,
            ],
            "ghostty is published after the binding built against it, then in pin order"
        );
        let only = selected(Some(GHOSTTY)).expect("selected");
        assert_eq!(
            plan(&only, false).ok(),
            Some(vec![
                Take(GHOSTTY),
                Take(LIBGHOSTTY_RS),
                Push(GHOSTTY),
                MoveSubmodule,
                Push(LIBGHOSTTY_RS),
                MovePins
            ]),
            "--only ghostty"
        );
        assert_eq!(
            plan(&[LIBGHOSTTY_RS], false).ok(),
            Some(vec![Take(LIBGHOSTTY_RS), Push(LIBGHOSTTY_RS), MovePins]),
            "the binding alone pins vendor/ghostty as it stands and leaves it there"
        );
        assert_eq!(
            plan(&only, true).ok(),
            Some(vec![Take(GHOSTTY), Take(LIBGHOSTTY_RS)]),
            "--no-push takes and checks, and neither pushes nor moves a pin"
        );
        assert!(plan(&[GHOSTTY], false).is_err(), "ghostty without the binding that pins it");
    }

    #[test]
    fn only_the_pinned_forks_move_the_lock() {
        let packages: Vec<&str> = ORDER.iter().filter_map(|fork| lock_package(fork)).collect();
        assert_eq!(packages, ["gpui", "gpui-kit", "libghostty-vt"], "ghostty is not in the lock");
    }

    #[test]
    fn a_move_counts_when_it_touches_the_paths_or_cannot_be_ruled_out() {
        let paths: Vec<String> =
            ["src/terminal/", "build.zig.zon"].into_iter().map(str::to_owned).collect();
        let files = |names: &[&str]| names.iter().copied().map(str::to_owned).collect::<Vec<_>>();
        let app = files(&["macos/Sources/App.swift", "src/apprt/gtk/App.zig", "build.zig"]);
        assert!(!relevant(&app, &paths, 300), "the app alone is no news");
        let core = files(&["src/apprt/gtk/App.zig", "src/terminal/Parser.zig"]);
        assert!(relevant(&core, &paths, 300), "the parser is");
        assert_eq!(touching(&core, &paths), ["src/terminal/Parser.zig"], "named");
        assert!(relevant(&files(&["build.zig.zon"]), &paths, 300), "an exact path");
        assert!(!relevant(&files(&["build.zig.zon.json"]), &paths, 300), "not a prefix of a file");
        assert!(!relevant(&files(&["src/terminal2/x.zig"]), &paths, 300), "a directory ends in /");
        assert!(relevant(&app, &paths, 3), "a list cut at the cap may hide the rest");
        assert!(relevant(&app, &[], 300), "no paths: everything counts");
        assert!(!relevant(&[], &paths, 300), "nothing changed");
    }

    #[test]
    fn remotes_match_across_transports() {
        let fork = "https://github.com/aislopware/gpui-fast.git";
        assert!(same_repo("git@github.com:aislopware/gpui-fast.git", fork), "scp-style SSH");
        assert!(same_repo("ssh://git@github.com/aislopware/gpui-fast", fork), "SSH URL");
        assert!(same_repo("https://github.com/aislopware/gpui-fast/", fork), "no .git, slash");
        assert!(!same_repo("https://github.com/longbridge/gpui-fast", fork), "another owner");
        assert!(same_repo("/tmp/zed", "/tmp/zed/.git"), "local paths");
        assert_eq!(
            repo_slug("https://github.com/longbridge/gpui-fast"),
            "longbridge/gpui-fast",
            "slug"
        );
    }

    #[test]
    fn lock_pin_reads_the_git_source() {
        let lock = "[[package]]\nname = \"gpui\"\n\
                    source = \"git+https://github.com/aislopware/gpui-fast#abc\"\n\
                    [[package]]\nname = \"gpui-kit\"\n\
                    source = \"git+https://github.com/aislopware/gpui-kit.git?branch=main#def\"\n";
        assert_eq!(
            lock_pin_in(lock, "https://github.com/aislopware/gpui-fast.git").as_deref(),
            Some("abc"),
            "sha after the fragment, with or without .git"
        );
        assert_eq!(
            lock_pin_in(lock, "https://github.com/aislopware/gpui-kit.git").as_deref(),
            Some("def"),
            "a query is not part of the repository"
        );
        assert_eq!(lock_pin_in(lock, "https://github.com/aislopware/zed.git"), None, "no pin");
    }
}

//! What this worker has, as the [`Facts`] an orchestrator reads to pick a worker.
//!
//! The agents and toolchains installed, the GPUs and the power source
//! (`docs/decisions/projects.md`), and the clones the server had made under
//! `~/slopty/clones` ([`REPOS`]), so a server that restarts still finds them.
//!
//! The server fills in what it knows itself, from the worker's registration and capabilities
//! (`os`, `arch`, `cpus`, `memory_mb`, `encoders`, `displays`, `load` and the rest), so none of
//! those is here.
//!
//! None of this is on a path anyone waits on. [`watch()`] gathers on a task of its own, once
//! someone first reads the facts ([`Kept::asked`]) and every [`REFRESH`] after, and publishes a
//! map only when it differs from the last. Every command is bounded by a timeout, the
//! independent ones run at once, and each runs at utility priority in a process group of its
//! own, which a timeout kills whole, and the warden too once the worker is gone
//! ([`crate::warden`]). A tool that is missing or does not answer in time is only absent.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use slopty_agent::managed::{self, Launcher};
use slopty_core::WallMs;
use slopty_proto::server::{Fact, Facts};
use tokio::io::AsyncReadExt as _;
use tokio::sync::watch;
use tokio::task::JoinSet;

/// How often the facts are gathered again: installs and upgrades are rare.
pub const REFRESH: Duration = Duration::from_mins(10);
/// The fact the clones under `~/slopty/clones` are in: each key of a clone's identity
/// ([`slopty_proto::terminal::RepoId::keys`]) to its path, as the server keys its `repos`.
pub const REPOS: &str = "repos";
/// How deep under `~/slopty/clones` a clone is looked for: `host/owner/repo`, with room for a
/// GitLab group's subgroups.
const CLONES_DEPTH: usize = 6;
/// The most clones listed.
const CLONES_MAX: usize = 256;
/// How long a `--version` may take: an agent written in Node or Python starts slowly.
const VERSION_WAIT: Duration = Duration::from_secs(10);
/// How much of a `--version` is read, in bytes.
const VERSION_OUTPUT_MAX: usize = 4096;
/// The longest version text kept, in characters.
const VERSION_MAX: usize = 128;
/// How long the login shell may take to say its `PATH`, profile and rc files included.
const LOGIN_WAIT: Duration = Duration::from_secs(5);
/// How much of a listing (`PATH`, the Rust targets, the GPUs) is read, in bytes.
const LISTING_MAX: usize = 64 * 1024;
/// What brackets the login shell's `PATH` in its output, which an rc file may print into.
const PATH_MARK: &str = "__SLOPTY_PATH__";

/// A program whose version is a fact.
struct Tool {
    /// Its key in the map.
    name: &'static str,
    /// What is run.
    program: &'static str,
    /// What makes it print its version.
    args: &'static [&'static str],
    /// The words its version follows, where another version comes first.
    after: Option<&'static str>,
}

impl Tool {
    const fn version(name: &'static str, program: &'static str) -> Self {
        Self { name, program, args: &["--version"], after: None }
    }
}

/// The coding agents' command lines, under `agents`.
const AGENTS: [Tool; 4] = [
    Tool::version("claude", "claude"),
    Tool::version("codex", "codex"),
    Tool::version("pi", "pi"),
    Tool::version("aider", "aider"),
];

/// The toolchains, under `toolchains`.
const TOOLCHAINS: &[Tool] = &[
    Tool::version("rustc", "rustc"),
    Tool::version("cargo", "cargo"),
    Tool::version("docker", "docker"),
    Tool::version("node", "node"),
    Tool::version("bun", "bun"),
    Tool::version("python", "python3"),
    Tool { name: "go", program: "go", args: &["version"], after: None },
    // `swift-driver version: 1.127.14.1 Apple Swift version 6.2 (…)`.
    Tool { name: "swift", program: "swift", args: &["--version"], after: Some("Swift version") },
    Tool::version("java", "java"),
    Tool::version("git", "git"),
];

/// How long a reader waits for the facts gathered the first time ([`Kept::asked`]); what is
/// still missing then follows as it comes.
pub const ASKED_WAIT: Duration = Duration::from_secs(15);

/// This worker's facts, and the ask that has them gathered.
///
/// Nothing is probed until someone reads them ([`Self::asked`]): the server asks once a client
/// or an agent reads the fleet's facts. Every worker a test starts would otherwise run a dozen
/// probes it never needed, and a worker would start with them. Only the clones are known from
/// the start, being a walk of one folder, so a task is placed beside one before anybody asked.
#[derive(Clone, Debug, Default)]
pub struct Kept {
    facts: watch::Sender<Facts>,
    wanted: Arc<tokio::sync::Notify>,
    gathered: watch::Sender<bool>,
}

impl Kept {
    /// What follows the facts as they change.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Facts> {
        self.facts.subscribe()
    }

    /// The facts, gathered for the first time on the first ask: waits for that up to `wait`,
    /// and answers what is known then.
    pub async fn asked(&self, wait: Duration) -> Facts {
        self.wanted.notify_one();
        let mut gathered = self.gathered.subscribe();
        let _in_time = tokio::time::timeout(wait, gathered.wait_for(|done| *done)).await;
        self.facts.borrow().clone()
    }

    /// The server had a clone of `repo` made at `path`: [`REPOS`] holds it from now on, before
    /// the next gathering finds it.
    pub fn clone_made(&self, path: &Path, repo: &slopty_proto::terminal::RepoId) {
        clone_made(&self.facts, path, repo);
    }
}

/// Keep `kept` current until nobody follows it.
///
/// The clones are known at once; everything else once it is first asked for ([`Kept::asked`])
/// and every [`REFRESH`] after.
pub async fn watch(kept: Kept) {
    let Kept { facts, wanted, gathered } = kept;
    let root = crate::repo::cloning::clones_root(&slopty_platform::dirs::home());
    let mut first = Facts::from([(REPOS.to_owned(), clones(&root).await)]);
    with_made(&facts, &mut first).await;
    publish(&facts, first);
    tokio::select! {
        () = wanted.notified() => {}
        () = facts.closed() => return,
    }
    let mut tick = tokio::time::interval(REFRESH);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if facts.is_closed() {
            return;
        }
        let mut next = gather().await;
        with_made(&facts, &mut next).await;
        publish(&facts, next);
        gathered.send_replace(true);
    }
}

/// `next` with every clone made while it was gathered ([`clone_made`]), until its folder is
/// gone.
async fn with_made(facts: &watch::Sender<Facts>, next: &mut Facts) {
    let made_meanwhile = facts.borrow().get(REPOS).cloned();
    if let (Some(Fact::Map(made)), Some(Fact::Map(found))) = (made_meanwhile, next.get_mut(REPOS)) {
        let there = tokio::task::spawn_blocking(move || still_there(made)).await;
        for (key, path) in there.unwrap_or_default() {
            found.entry(key).or_insert(path);
        }
    }
}

/// The server had a clone of `repo` made at `path`: [`REPOS`] holds it from now on, before the
/// next gathering finds it.
fn clone_made(facts: &watch::Sender<Facts>, path: &Path, repo: &slopty_proto::terminal::RepoId) {
    let path = path.to_string_lossy().into_owned();
    facts.send_if_modified(|now| {
        let repos = now.entry(REPOS.to_owned()).or_insert_with(|| Fact::Map(Facts::new()));
        let Fact::Map(repos) = repos else { return false };
        let mut changed = false;
        for key in repo.keys() {
            let at = Fact::Text(path.clone());
            changed |= repos.insert(key.to_owned(), at.clone()) != Some(at);
        }
        changed
    });
}

/// The entries of a [`REPOS`] map whose clone's folder is still there.
fn still_there(repos: Facts) -> Vec<(String, Fact)> {
    repos
        .into_iter()
        .filter(|(_, path)| matches!(path, Fact::Text(p) if Path::new(p).is_dir()))
        .collect()
}

/// The clones under `root` (`~/slopty/clones`), as [`REPOS`] lists them.
///
/// Every folder holding a `.git` at most `CLONES_DEPTH` below it, at most `CLONES_MAX` of
/// them, each by the keys of its identity. A key two clones share names the first path in
/// order.
pub async fn clones(root: &Path) -> Fact {
    let root = root.to_path_buf();
    let found = tokio::task::spawn_blocking(move || clone_roots(&root)).await.unwrap_or_default();
    let mut repos = Facts::new();
    for path in found {
        let id = crate::repo::identify(path.clone()).await;
        let text = path.to_string_lossy().into_owned();
        for key in id.keys() {
            repos.entry(key.to_owned()).or_insert_with(|| Fact::Text(text.clone()));
        }
    }
    Fact::Map(repos)
}

/// The folders under `root` holding a `.git`, in order, not looking inside one.
fn clone_roots(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut next = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = next.pop() {
        if found.len() >= CLONES_MAX {
            break;
        }
        if depth > 0 && dir.join(".git").exists() {
            found.push(dir);
            continue;
        }
        if depth >= CLONES_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut subdirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            // A clone under way is made beside its place under a hidden name.
            .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
            .collect();
        subdirs.sort();
        next.extend(subdirs.into_iter().rev().map(|p| (p, depth.saturating_add(1))));
    }
    found.sort();
    found
}

/// Make `next` the current facts, telling the watchers only when it differs; whether it did.
pub fn publish(facts: &watch::Sender<Facts>, next: Facts) -> bool {
    facts.send_if_modified(|current| {
        let changed = *current != next;
        if changed {
            *current = next;
        }
        changed
    })
}

/// Everything this worker reports of itself now.
///
/// `agents` and `toolchains` are always there, empty when nothing is, so a reader can ask what
/// is in them.
pub async fn gather() -> Facts {
    let (login, stand_ins) = tokio::join!(login_path(), StandIns::find());
    let search = Arc::new(SearchPath::of(std::env::var_os("PATH").into_iter().chain(login)));
    let clones_root = crate::repo::cloning::clones_root(&slopty_platform::dirs::home());
    let (agents, toolchains, rust_targets, gpus, power, repos) = tokio::join!(
        versions(&search, &stand_ins, &AGENTS),
        versions(&search, &stand_ins, TOOLCHAINS),
        rust_targets(),
        gpus(&search),
        power(),
        clones(&clones_root),
    );
    let mut toolchains = toolchains;
    if let Some(xcode) = stand_ins.xcode {
        toolchains.insert("xcode".to_owned(), Fact::Text(xcode));
    }
    let mut facts = Facts::new();
    facts.insert("agents".to_owned(), Fact::Map(agents));
    facts.insert("toolchains".to_owned(), Fact::Map(toolchains));
    facts.insert(REPOS.to_owned(), repos);
    let texts = |items: Vec<String>| Fact::List(items.into_iter().map(Fact::Text).collect());
    if let Some(targets) = rust_targets {
        facts.insert("rust_targets".to_owned(), texts(targets));
    }
    if let Some(gpus) = gpus {
        facts.insert("gpus".to_owned(), texts(gpus));
    }
    if let Some(power) = power {
        facts.insert("power".to_owned(), Fact::Text(power.to_owned()));
    }
    facts
}

/// The directories agents are looked for in: the worker's own `PATH`, then the login shell's,
/// each once.
pub async fn agent_dirs() -> Vec<PathBuf> {
    let login = login_path().await;
    SearchPath::of(std::env::var_os("PATH").into_iter().chain(login)).dirs
}

/// The coding agents installed in `dirs` ([`agent_dirs`]), by program, as `gather` puts them
/// under `agents`: what the worker's capabilities say it can start
/// ([`crate::caps::installed_agents`]).
pub async fn agents(dirs: &[PathBuf]) -> Facts {
    let stand_ins = StandIns::find().await;
    let lists = std::iter::once(std::env::join_paths(dirs).unwrap_or_default());
    let search = Arc::new(SearchPath::of(lists));
    versions(&search, &stand_ins, &AGENTS).await
}

/// The versions of those of `tools` installed here, by name.
async fn versions(search: &Arc<SearchPath>, stand_ins: &StandIns, tools: &'static [Tool]) -> Facts {
    let mut running = JoinSet::new();
    for tool in tools {
        let Some(program) = search.resolve(tool.program).filter(|p| stand_ins.answer(p)) else {
            continue;
        };
        let search = Arc::clone(search);
        running.spawn(async move {
            let version = if tool.program == CLAUDE {
                claude_version(&search, &program, &managed_data()).await?
            } else {
                let ran =
                    run(search.command(&program, tool.args), VERSION_WAIT, VERSION_OUTPUT_MAX)
                        .await?;
                ran.ok.then(|| version_in(&ran.out, tool.after)).flatten()?
            };
            Some((tool.name.to_owned(), Fact::Text(version)))
        });
    }
    let mut found = Facts::new();
    while let Some(done) = running.join_next().await {
        if let Ok(Some((name, version))) = done {
            found.insert(name, version);
        }
    }
    found
}

/// The version in what a program printed: the first word shaped like one (`1.93.0`, `v22.1.0`,
/// `go1.24.1`), after `after` when it is given and found; else the first line.
fn version_in(text: &str, after: Option<&str>) -> Option<String> {
    let from = after.and_then(|words| text.find(words).map(|at| at.saturating_add(words.len())));
    let rest = text.get(from.unwrap_or(0)..).unwrap_or(text);
    let dotted = rest
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | ':'))
        .find_map(|word| {
            let version = word.get(word.find(|c: char| c.is_ascii_digit())?..)?;
            let (_, minor) = version.split_once('.')?;
            minor.starts_with(|c: char| c.is_ascii_digit()).then_some(version)
        });
    let found = dotted.or_else(|| text.lines().map(str::trim).find(|line| !line.is_empty()))?;
    Some(found.chars().take(VERSION_MAX).collect())
}

/// The Rust targets the active toolchain has a standard library for, as `rustup target list
/// --installed` lists them, when rustup is here.
///
/// They are read from rustup's own files, never by running it: rustup in a folder whose
/// `rust-toolchain.toml` names a toolchain it lacks installs that first, which on a CI Mac held
/// gigabytes for minutes, once for every worker a test started.
async fn rust_targets() -> Option<Vec<String>> {
    let home = std::env::var_os("RUSTUP_HOME")
        .filter(|home| !home.is_empty())
        .map_or_else(|| slopty_platform::dirs::home().join(".rustup"), PathBuf::from);
    let named = std::env::var("RUSTUP_TOOLCHAIN").ok().filter(|name| !name.is_empty());
    let here = std::env::current_dir().ok();
    let read = move || installed_targets(&home, named.as_deref(), here.as_deref());
    tokio::task::spawn_blocking(read).await.ok().flatten()
}

/// rustup's `settings.toml`, as much of it as says which toolchain is active.
#[derive(Debug, Default, serde::Deserialize)]
struct Rustup {
    default_host_triple: Option<String>,
    default_toolchain: Option<String>,
    /// A folder's toolchain, set by `rustup override`.
    #[serde(default)]
    overrides: BTreeMap<String, String>,
}

/// A `rust-toolchain.toml`.
#[derive(Debug, serde::Deserialize)]
struct ToolchainFile {
    toolchain: PinnedToolchain,
}

#[derive(Debug, serde::Deserialize)]
struct PinnedToolchain {
    channel: Option<String>,
    path: Option<String>,
}

/// The targets of the toolchain rustup under `home` takes as active, the way it chooses: the
/// one `named` (`RUSTUP_TOOLCHAIN`), else the nearest of a folder's override and its
/// `rust-toolchain` file from `here` up, else the default. `None` when there is none, or it is
/// not installed.
fn installed_targets(home: &Path, named: Option<&str>, here: Option<&Path>) -> Option<Vec<String>> {
    let rustup = std::fs::read_to_string(home.join("settings.toml"))
        .ok()
        .and_then(|text| toml::from_str::<Rustup>(&text).ok())
        .unwrap_or_default();
    let pinned = || here?.ancestors().find_map(|dir| pinned_in(dir, &rustup));
    let name = named.map(str::to_owned).or_else(pinned).or(rustup.default_toolchain)?;
    let toolchain = toolchain_dir(home, &name, rustup.default_host_triple.as_deref())?;
    let components = std::fs::read_to_string(toolchain.join("lib/rustlib/components")).ok()?;
    let mut targets: Vec<String> = components
        .lines()
        .filter_map(|component| component.trim().strip_prefix("rust-std-"))
        .map(str::to_owned)
        .collect();
    targets.sort();
    targets.dedup();
    Some(targets)
}

/// The toolchain `dir` pins: its `rustup override`, else its `rust-toolchain.toml` or
/// `rust-toolchain` file (TOML, or the bare name the old form holds).
fn pinned_in(dir: &Path, rustup: &Rustup) -> Option<String> {
    if let Some(name) = rustup.overrides.get(dir.to_string_lossy().as_ref()) {
        return Some(name.clone());
    }
    ["rust-toolchain.toml", "rust-toolchain"].into_iter().find_map(|file| {
        let text = std::fs::read_to_string(dir.join(file)).ok()?;
        match toml::from_str::<ToolchainFile>(&text) {
            Ok(ToolchainFile { toolchain }) => toolchain.path.or(toolchain.channel),
            Err(_) => {
                Some(text.trim().to_owned()).filter(|name| !name.is_empty() && !name.contains('\n'))
            }
        }
    })
}

/// Where the toolchain `name` is installed under `home`: a path as it is, else its own folder,
/// else the folder of its name for `host` (`stable` is `stable-aarch64-apple-darwin`), else the
/// first one for any host.
fn toolchain_dir(home: &Path, name: &str, host: Option<&str>) -> Option<PathBuf> {
    if Path::new(name).is_absolute() {
        return Some(PathBuf::from(name));
    }
    let toolchains = home.join("toolchains");
    let exact = toolchains.join(name);
    if exact.is_dir() {
        return Some(exact);
    }
    let hosted = host.map(|host| toolchains.join(format!("{name}-{host}")));
    if let Some(hosted) = hosted.filter(|dir| dir.is_dir()) {
        return Some(hosted);
    }
    let prefix = format!("{name}-");
    let mut any: Vec<PathBuf> = std::fs::read_dir(&toolchains)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
        .collect();
    any.sort();
    any.into_iter().next()
}

/// The chip, whose GPU it is (`Apple M2 Ultra`).
#[cfg(target_os = "macos")]
async fn gpus(search: &SearchPath) -> Option<Vec<String>> {
    let brand = search.command(Path::new("/usr/sbin/sysctl"), &["-n", "machdep.cpu.brand_string"]);
    let ran = run(brand, VERSION_WAIT, LISTING_MAX).await.filter(|ran| ran.ok)?;
    (!ran.out.is_empty()).then(|| vec![ran.out])
}

/// NVIDIA's GPUs, when `nvidia-smi` is here.
#[cfg(not(target_os = "macos"))]
async fn gpus(search: &SearchPath) -> Option<Vec<String>> {
    let smi = search.resolve("nvidia-smi")?;
    let ran = run(search.command(&smi, &["-L"]), VERSION_WAIT, LISTING_MAX).await;
    Some(nvidia_gpus(&ran.filter(|ran| ran.ok)?.out))
}

/// The names in `nvidia-smi -L`: `GPU 0: NVIDIA A100-SXM4-40GB (UUID: GPU-…)`.
#[cfg(any(not(target_os = "macos"), test))]
fn nvidia_gpus(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.split_once(": ")?;
            Some(rest.split(" (UUID").next().unwrap_or(rest).trim().to_owned())
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// `ac` or `battery`, from the power source `pmset` says it draws from.
#[cfg(target_os = "macos")]
async fn power() -> Option<&'static str> {
    let search = SearchPath::of(None);
    let batt = search.command(Path::new("/usr/bin/pmset"), &["-g", "batt"]);
    let ran = run(batt, VERSION_WAIT, LISTING_MAX).await.filter(|ran| ran.ok)?;
    power_source(&ran.out)
}

/// `Now drawing from 'AC Power'`.
#[cfg(any(target_os = "macos", test))]
fn power_source(batt: &str) -> Option<&'static str> {
    let line = batt.lines().next()?;
    if line.contains("'AC Power'") {
        Some("ac")
    } else if line.contains("'Battery Power'") {
        Some("battery")
    } else {
        None
    }
}

/// `ac` while a mains supply is online, `battery` when one is offline and a battery is here.
#[cfg(not(target_os = "macos"))]
async fn power() -> Option<&'static str> {
    let mut supplies = tokio::fs::read_dir("/sys/class/power_supply").await.ok()?;
    let (mut mains, mut battery) = (None, false);
    while let Ok(Some(supply)) = supplies.next_entry().await {
        let read = async |name: &str| {
            tokio::fs::read_to_string(supply.path().join(name)).await.unwrap_or_default()
        };
        match read("type").await.trim() {
            "Mains" => mains = Some(mains.unwrap_or(false) || read("online").await.trim() == "1"),
            "Battery" => battery = true,
            _ => {}
        }
    }
    match mains {
        Some(true) => Some("ac"),
        Some(false) if battery => Some("battery"),
        _ => None,
    }
}

/// Where programs are looked for: the daemon's own `PATH`, then the person's login shell's,
/// which is where a daemon launchd started finds what they installed.
#[derive(Clone, Debug)]
struct SearchPath {
    dirs: Vec<PathBuf>,
    joined: OsString,
}

impl SearchPath {
    /// `lists` of directories, `PATH`-style, as one, the first place of each kept.
    fn of(lists: impl IntoIterator<Item = OsString>) -> Self {
        let mut dirs: Vec<PathBuf> = Vec::new();
        for list in lists {
            for dir in std::env::split_paths(&list) {
                if dir.is_absolute() && !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
        }
        let joined = std::env::join_paths(&dirs).unwrap_or_default();
        Self { dirs, joined }
    }

    /// The first executable file called `program` in these directories.
    fn resolve(&self, program: &str) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt as _;
        self.dirs.iter().map(|dir| dir.join(program)).find(|path| {
            path.metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
    }

    /// `program args…` at a lower priority, finding what it runs in these directories.
    fn command(&self, program: &Path, args: &[&str]) -> tokio::process::Command {
        let mut command = low_priority(self, program);
        command.args(args);
        if !self.joined.is_empty() {
            command.env("PATH", &self.joined);
        }
        command
    }
}

/// `program` run at utility priority: behind every thread a person waits on. Not background,
/// which keeps it to the efficiency cores: two of them on some Macs, where a JVM and a few Node
/// programs starting at once ran past [`VERSION_WAIT`].
#[cfg(target_os = "macos")]
fn low_priority(_search: &SearchPath, program: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("/usr/sbin/taskpolicy");
    command.args(["-c", "utility"]).arg(program);
    command
}

/// `program` run at a lower priority, through `nice` where it is found.
#[cfg(not(target_os = "macos"))]
fn low_priority(search: &SearchPath, program: &Path) -> tokio::process::Command {
    let Some(nice) = search.resolve("nice") else { return tokio::process::Command::new(program) };
    let mut command = tokio::process::Command::new(nice);
    command.args(["-n", "10"]).arg(program);
    command
}

/// A program the person installed, as their terminal finds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// The program.
    pub program: PathBuf,
    /// The `PATH` it runs with, which finds what it runs in turn (a Node program's `node`).
    pub path: OsString,
    /// What it says its version is, when it says.
    pub version: Option<String>,
}

/// `program` as the person's terminal finds it, with the version it says.
///
/// It is looked for on the daemon's own `PATH`, then on the person's login shell's, which is
/// where a daemon launchd started finds what they installed; only on `path` when it is given.
/// `None` when it is not found. Its version is asked as [`gather`] asks it.
pub async fn installed(program: &str, path: Option<OsString>) -> Option<Installed> {
    let search = match path {
        Some(path) => SearchPath::of([path]),
        None => SearchPath::of(std::env::var_os("PATH").into_iter().chain(login_path().await)),
    };
    let found = search.resolve(program)?;
    let version = if program == CLAUDE {
        claude_version(&search, &found, &managed_data()).await
    } else {
        let ran =
            run(search.command(&found, &["--version"]), VERSION_WAIT, VERSION_OUTPUT_MAX).await;
        ran.filter(|ran| ran.ok).and_then(|ran| version_in(&ran.out, None))
    };
    Some(Installed { program: found, path: search.joined, version })
}

/// Claude Code's program.
const CLAUDE: &str = "claude";

/// A `claude` found: its real path and when it was last written.
type Found = (PathBuf, Option<SystemTime>);

/// What each `claude` found is: asked once for the worker's life, and again only when it is
/// replaced ([`launcher`]).
static LAUNCHERS: LazyLock<Mutex<HashMap<Found, Launcher>>> = LazyLock::new(Mutex::default);

/// Where a managed launcher keeps its clients: the person's data directory.
fn managed_data() -> PathBuf {
    slopty_platform::dirs::home().join(".local").join("share")
}

/// The version of the `claude` at `program`, never by running a managed launcher
/// ([`slopty_agent::managed`]): a launcher's client says it in its path, a launcher by the
/// newest client under `data`; any other `claude` is asked `--version`.
async fn claude_version(search: &SearchPath, program: &Path, data: &Path) -> Option<String> {
    let real = tokio::fs::canonicalize(program).await.unwrap_or_else(|_| program.to_path_buf());
    if let Some(version) = managed::artifact_version(&real) {
        return Some(version);
    }
    if launcher(search, &real).await.is_managed() {
        let data = data.to_path_buf();
        return tokio::task::spawn_blocking(move || managed::newest_artifact(&data))
            .await
            .ok()
            .flatten();
    }
    let ran = run(search.command(program, &["--version"]), VERSION_WAIT, VERSION_OUTPUT_MAX).await;
    ran.filter(|ran| ran.ok).and_then(|ran| version_in(&ran.out, None))
}

/// Whether the `claude` at `program` (its real path) is a managed launcher, from its answer to
/// [`managed::MANAGED_HELP`], which such a launcher gives before any I/O and Claude Code
/// refuses at once. One that does not answer in time is taken for a launcher this once: it is
/// not asked anything else, and is asked again next time.
async fn launcher(search: &SearchPath, program: &Path) -> Launcher {
    let written = tokio::fs::metadata(program).await.and_then(|meta| meta.modified()).ok();
    let key = (program.to_path_buf(), written);
    let known = LAUNCHERS.lock().get(&key).copied();
    if let Some(known) = known {
        return known;
    }
    let asked = search.command(program, &[managed::MANAGED_HELP]);
    let Some(ran) = run(asked, VERSION_WAIT, VERSION_OUTPUT_MAX).await else {
        return Launcher::Managed;
    };
    let launcher = Launcher::from_managed_help(ran.ok, &ran.out);
    LAUNCHERS.lock().insert(key, launcher);
    launcher
}

/// The `PATH` the person's own programs run with: the daemon's own, then their login shell's,
/// each directory once.
///
/// A daemon launchd started has little on its `PATH`, so a git hook, git-lfs, a signing helper
/// or a pre-commit tool installed by Homebrew, mise or npm is found only on the login shell's.
/// The login shell is read once for the worker's life. While it does not answer, the daemon's
/// own `PATH` stands in, and it is asked again only `LOGIN_RETRY` after it last failed: each
/// ask waits up to `LOGIN_WAIT`, which a commit or a push would otherwise wait every time.
pub async fn person_path() -> OsString {
    static READ: tokio::sync::OnceCell<OsString> = tokio::sync::OnceCell::const_new();
    static FAILED_MS: AtomicU64 = AtomicU64::new(0);
    let own = || std::env::var_os("PATH").unwrap_or_default();
    if let Some(read) = READ.get() {
        return read.clone();
    }
    let failed = WallMs::from_millis(FAILED_MS.load(Ordering::Relaxed));
    if !ask_login_again(failed, WallMs::now()) {
        return own();
    }
    let read = READ
        .get_or_try_init(async || {
            let login = login_path().await.ok_or(())?;
            Ok::<_, ()>(SearchPath::of(std::env::var_os("PATH").into_iter().chain([login])).joined)
        })
        .await;
    if let Ok(read) = read {
        return read.clone();
    }
    if FAILED_MS.swap(WallMs::now().as_millis(), Ordering::Relaxed) == 0 {
        tracing::warn!(
            "the login shell did not say its PATH within {LOGIN_WAIT:?}: git runs on the \
             daemon's own, and the shell is asked again in {LOGIN_RETRY:?}"
        );
    }
    own()
}

/// How long a login shell that did not say its `PATH` is left before it is asked again.
const LOGIN_RETRY: Duration = Duration::from_mins(1);

/// Whether the login shell is asked again at `now`, having last failed at `failed` (zero when
/// it never has).
fn ask_login_again(failed: WallMs, now: WallMs) -> bool {
    failed.is_zero() || now.since(failed) >= LOGIN_RETRY
}

/// The person's login shell's `PATH`, as it stands once their profile and rc files ran.
async fn login_path() -> Option<OsString> {
    let shell =
        std::env::var_os("SHELL").filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let script = format!("printf '{PATH_MARK}%s{PATH_MARK}' \"$PATH\"");
    let mut login = tokio::process::Command::new(shell);
    login.args(["-l", "-i", "-c", &script]);
    let ran = run(login, LOGIN_WAIT, LISTING_MAX).await.filter(|ran| ran.ok)?;
    let (_, rest) = ran.out.split_once(PATH_MARK)?;
    let (path, _) = rest.split_once(PATH_MARK)?;
    Some(path.into())
}

/// Whether macOS's stand-ins in `/usr/bin` have something behind them. With nothing there, the
/// developer tools' shims and `java` put up an installer dialog instead of answering, so they
/// are not run.
#[derive(Clone, Debug)]
struct StandIns {
    developer: bool,
    java: bool,
    /// Xcode's version, when the developer directory is an Xcode's ([`xcode_version`]).
    xcode: Option<String>,
}

impl StandIns {
    #[cfg(target_os = "macos")]
    async fn find() -> Self {
        let search = SearchPath::of(None);
        let answers = async |program: &str, args: &[&str]| {
            let ran = run(search.command(Path::new(program), args), VERSION_WAIT, LISTING_MAX);
            ran.await.filter(|ran| ran.ok)
        };
        let (developer, java) = tokio::join!(
            answers("/usr/bin/xcode-select", &["-p"]),
            answers("/usr/libexec/java_home", &[]),
        );
        let xcode = developer.as_ref().and_then(|dir| xcode_version(Path::new(&dir.out)));
        Self { developer: developer.is_some(), java: java.is_some(), xcode }
    }

    /// Only macOS puts stand-ins in `/usr/bin`.
    #[cfg(not(target_os = "macos"))]
    fn find() -> impl Future<Output = Self> {
        std::future::ready(Self { developer: true, java: true, xcode: None })
    }

    /// Whether `program` answers rather than asking to install something.
    fn answer(&self, program: &Path) -> bool {
        match program.to_str() {
            Some("/usr/bin/java") => self.java,
            Some("/usr/bin/git" | "/usr/bin/python3" | "/usr/bin/swift") => self.developer,
            _ => true,
        }
    }
}

/// The version of the Xcode whose developer directory is `developer` (what `xcode-select -p`
/// prints), from the `version.plist` beside it; `None` for the command line tools alone, which
/// have none.
///
/// Never by running `xcodebuild -version`: after an Xcode update, until its components are
/// installed, any `xcodebuild` puts up a dialog asking for the person's password to install
/// them, and a worker probing every [`REFRESH`] put it up again and again.
#[cfg(target_os = "macos")]
fn xcode_version(developer: &Path) -> Option<String> {
    let plist = plist::Value::from_file(developer.parent()?.join("version.plist")).ok()?;
    let version = plist.as_dictionary()?.get("CFBundleShortVersionString")?.as_string()?;
    Some(version.to_owned())
}

/// How a command ended, when it did in time.
pub(crate) struct Ran {
    /// It exited with success.
    pub ok: bool,
    /// What it printed, up to the cap, trimmed.
    pub out: String,
}

/// Run `command` for at most `wait`, keeping the first `cap` bytes it prints.
///
/// It runs in a process group of its own, which is killed whole when it runs over or when the
/// caller stops waiting (a worker shutting down drops it), and by the warden when the worker is
/// gone ([`crate::warden`]): a tool's wrapper script dies with whatever it started. Past the cap
/// its output is read and dropped, so it finishes rather than blocking on a full pipe.
pub(crate) async fn run(
    mut command: tokio::process::Command,
    wait: Duration,
    cap: usize,
) -> Option<Ran> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = command.spawn().ok()?;
    // Declared after the child, so it drops first: the group is killed while its leader is
    // unreaped, and so still this child's.
    let mut group = crate::warden::Group::of(&child);
    let mut stdout = child.stdout.take()?;
    let finished = tokio::time::timeout(wait, async {
        let mut kept = Vec::new();
        let limit = u64::try_from(cap).unwrap_or(u64::MAX);
        (&mut stdout).take(limit).read_to_end(&mut kept).await.ok()?;
        tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await.ok()?;
        let status = child.wait().await.ok()?;
        // Reaped: its id may name another group from now on.
        group.reaped();
        Some(Ran { ok: status.success(), out: text_of(&kept) })
    })
    .await;
    finished.ok().flatten()
}

/// `bytes` as text, trimmed, without the half of a character a cap cut off.
fn text_of(bytes: &[u8]) -> String {
    let whole = match std::str::from_utf8(bytes) {
        Err(e) if e.error_len().is_none() => bytes.get(..e.valid_up_to()).unwrap_or_default(),
        _ => bytes,
    };
    String::from_utf8_lossy(whole).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// A login shell that did not answer is asked again only once a minute has passed, and one
    /// that never failed is always asked.
    #[test]
    fn a_login_shell_that_failed_is_left_a_minute() {
        let at =
            |s: u64| WallMs::from_millis(1_790_000_000_000).saturating_add(Duration::from_secs(s));
        assert!(ask_login_again(WallMs::ZERO, at(0)), "never failed");
        assert!(!ask_login_again(at(0), at(1)), "failed a second ago");
        assert!(!ask_login_again(at(0), at(59)));
        assert!(ask_login_again(at(0), at(60)), "a minute on");
    }

    fn here() -> SearchPath {
        SearchPath::of(std::env::var_os("PATH"))
    }

    /// `line` through `sh -c`, as a tool's wrapper script runs.
    async fn sh(line: &str, wait: Duration, cap: usize) -> Option<Ran> {
        run(here().command(Path::new("/bin/sh"), &["-c", line]), wait, cap).await
    }

    /// A command says what it printed, trimmed, and whether it succeeded.
    #[tokio::test]
    async fn a_command_says_what_it_printed_and_whether_it_succeeded() {
        let wait = Duration::from_secs(30);
        let said = sh("printf '  ok\\n\\n'", wait, 64).await.unwrap();
        assert!(said.ok && said.out == "ok", "trimmed: {:?}", said.out);
        let failed = sh("printf said; exit 3", wait, 64).await.unwrap();
        assert!(!failed.ok && failed.out == "said");
        assert!(!sh("no-such-program-anywhere", wait, 64).await.unwrap().ok);
        assert!(sh("echo \"$PATH\" | grep -q /", wait, 64).await.unwrap().ok, "PATH is set");
    }

    /// A command that runs over is none at its time, and what it started dies with it.
    ///
    /// On tokio's paused clock, which a blocking task holds still: it stands while the shell
    /// starts its `sleep` (however long a loaded machine takes), then the budget runs out at
    /// once, at exactly its time. On the wall clock a 500 ms budget once ran out before the shell
    /// had written the pid.
    #[tokio::test(start_paused = true)]
    async fn a_command_past_its_time_is_none_and_leaves_nothing_running() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let at = pid_file.display();
        let line = format!("sleep 30 & echo $! > '{at}.part'; mv '{at}.part' '{at}'; wait");
        let started = tokio::task::spawn_blocking({
            let pid_file = pid_file.clone();
            move || real_wait(|| pid_file.exists())
        });
        let budget = Duration::from_millis(500);
        let start = tokio::time::Instant::now();
        let (ran, started) = tokio::join!(sh(&line, budget, 64), started);
        assert!(started.unwrap(), "the shell started its sleep");
        assert!(ran.is_none(), "past its time");
        let took = start.elapsed();
        let tick = Duration::from_millis(1);
        assert!(took >= budget && took <= budget.saturating_add(tick), "at its time: {took:?}");
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        let pid = rustix::process::Pid::from_raw(pid).unwrap();
        let gone = move || rustix::process::test_kill_process(pid).is_err();
        assert!(tokio::task::spawn_blocking(move || real_wait(gone)).await.unwrap(), "killed");
    }

    /// Whether `done` holds within ten seconds of the wall clock, asked every 10 ms: for what
    /// a real process does, which tokio's clock does not see.
    #[expect(clippy::disallowed_methods, reason = "waits on a real process, off the runtime")]
    fn real_wait(mut done: impl FnMut() -> bool) -> bool {
        (0..1_000).any(|_| {
            let now = done();
            if !now {
                std::thread::sleep(Duration::from_millis(10));
            }
            now
        })
    }

    /// A command's output is cut at the cap, and the rest is drained so it still finishes.
    #[tokio::test]
    async fn a_long_output_is_cut_at_the_cap() {
        let line = "head -c 1000000 /dev/zero | tr '\\0' a";
        // The cap is under test, not the clock: a command runs at utility priority, which a
        // loaded machine starves.
        let ran = sh(line, Duration::from_secs(120), 1024).await.unwrap();
        assert_eq!(ran.out.len(), 1024);
        assert!(ran.ok && ran.out.bytes().all(|b| b == b'a'));
    }

    /// A cap that cuts a character in half drops the half, not the text.
    #[test]
    fn a_cut_character_is_dropped() {
        assert_eq!(text_of("héllo".as_bytes().get(..2).unwrap()), "h");
        assert_eq!(text_of(b" ok\n"), "ok");
        assert_eq!(text_of(b"a\xffb"), "a\u{fffd}b", "a bad byte inside stays visible");
    }

    #[test]
    fn a_version_is_found_in_what_a_tool_prints() {
        let cases = [
            ("rustc 1.93.0-nightly (abc 2026-09-01)", "1.93.0-nightly"),
            ("cargo 1.93.0 (1d8b05cdd 2026-08-01)", "1.93.0"),
            ("Xcode 26.5\nBuild version 17F5\n", "26.5"),
            ("Docker version 28.0.1, build 068a01e", "28.0.1"),
            ("v22.1.0\n", "22.1.0"),
            ("Python 3.13.1", "3.13.1"),
            ("go version go1.24.1 darwin/arm64", "1.24.1"),
            ("openjdk 17.0.12 2024-07-16 LTS\nOpenJDK Runtime", "17.0.12"),
            ("git version 2.50.1 (Apple Git-155)", "2.50.1"),
            ("2.1.283 (Claude Code)", "2.1.283"),
            ("codex-cli 0.40.0", "0.40.0"),
            ("2025.09.18-7ae6800", "2025.09.18-7ae6800"),
        ];
        for (text, want) in cases {
            assert_eq!(version_in(text, None).as_deref(), Some(want), "{text}");
        }
        let swift = "swift-driver version: 1.127.14.1 Apple Swift version 6.2 (swiftlang-6.2)";
        assert_eq!(version_in(swift, Some("Swift version")).as_deref(), Some("6.2"));
        assert_eq!(version_in("nightly build\n", None).as_deref(), Some("nightly build"));
        assert_eq!(version_in(" \n", None), None);
    }

    /// Only a changed map is sent.
    #[test]
    fn unchanged_facts_are_not_sent_again() {
        let (facts, mut seen) = watch::channel(Facts::new());
        let one = Facts::from([("arch".to_owned(), Fact::Text("aarch64".to_owned()))]);
        assert!(publish(&facts, one.clone()));
        assert!(seen.has_changed().unwrap());
        assert_eq!(*seen.borrow_and_update(), one);
        assert!(!publish(&facts, one), "the same map again");
        assert!(!seen.has_changed().unwrap(), "so nobody hears of it");
    }

    #[test]
    fn a_search_path_keeps_the_first_of_each_directory() {
        let search = SearchPath::of(["/bin:/usr/bin".into(), "/usr/bin:relative:/bin".into()]);
        assert_eq!(search.dirs, [Path::new("/bin"), Path::new("/usr/bin")]);
        assert_eq!(search.resolve("sh").as_deref(), Some(Path::new("/bin/sh")));
        assert_eq!(search.resolve("no-such-program-anywhere"), None);
    }

    /// The Rust targets are the active toolchain's, read from rustup's files and chosen as rustup
    /// chooses: `RUSTUP_TOOLCHAIN`, else the nearest override or toolchain file up from the
    /// folder, else the default; a toolchain that is not installed has none.
    #[test]
    fn the_rust_targets_are_the_active_toolchain_s_read_from_rustup_s_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("rustup");
        let host = "aarch64-apple-darwin";
        let toolchain = |name: &str, components: &str| {
            let rustlib = home.join("toolchains").join(name).join("lib/rustlib");
            std::fs::create_dir_all(&rustlib).unwrap();
            std::fs::write(rustlib.join("components"), components).unwrap();
        };
        toolchain(&format!("stable-{host}"), "cargo-x\nrust-std-aarch64-apple-darwin\nrustc-x\n");
        toolchain(
            &format!("nightly-{host}"),
            "rust-std-wasm32-wasip2\nrust-std-aarch64-apple-darwin\nrust-src\n",
        );
        toolchain("1.93.0-x86_64-unknown-linux-gnu", "rust-std-x86_64-unknown-linux-gnu\n");
        let pinned = tmp.path().join("pinned");
        let overridden = tmp.path().join("overridden");
        let legacy = tmp.path().join("legacy");
        let missing = tmp.path().join("missing");
        for dir in [&pinned, &overridden, &legacy, &missing] {
            std::fs::create_dir_all(dir.join("deep")).unwrap();
        }
        std::fs::write(pinned.join("rust-toolchain.toml"), "[toolchain]\nchannel = \"nightly\"\n")
            .unwrap();
        std::fs::write(legacy.join("rust-toolchain"), "1.93.0\n").unwrap();
        std::fs::write(missing.join("rust-toolchain.toml"), "[toolchain]\nchannel = \"beta\"\n")
            .unwrap();
        let settings = format!(
            "default_host_triple = \"{host}\"\ndefault_toolchain = \"stable-{host}\"\n\
             profile = \"default\"\nversion = \"12\"\n\n[overrides]\n\"{}\" = \"nightly-{host}\"\n",
            overridden.display()
        );
        std::fs::write(home.join("settings.toml"), settings).unwrap();

        let targets =
            |named: Option<&str>, here: &Path| installed_targets(&home, named, Some(here));
        let stable = Some(vec!["aarch64-apple-darwin".to_owned()]);
        let nightly = Some(vec!["aarch64-apple-darwin".to_owned(), "wasm32-wasip2".to_owned()]);
        assert_eq!(targets(None, tmp.path()), stable, "the default");
        assert_eq!(targets(None, &pinned.join("deep")), nightly, "the file up from the folder");
        assert_eq!(targets(None, &overridden.join("deep")), nightly, "the override");
        assert_eq!(
            targets(None, &legacy.join("deep")),
            Some(vec!["x86_64-unknown-linux-gnu".to_owned()]),
            "the old file's bare name, found for its own host"
        );
        assert_eq!(targets(Some("nightly"), &legacy), nightly, "RUSTUP_TOOLCHAIN over all");
        assert_eq!(targets(None, &missing), None, "a toolchain not installed has none");
        assert_eq!(installed_targets(&tmp.path().join("none"), None, None), None, "no rustup");
    }

    /// macOS's stand-ins run only with something behind them; every other program runs.
    #[test]
    fn a_stand_in_with_nothing_behind_it_is_not_run() {
        let bare = StandIns { developer: false, java: false, xcode: None };
        assert!(!bare.answer(Path::new("/usr/bin/git")));
        assert!(!bare.answer(Path::new("/usr/bin/java")));
        assert!(bare.answer(Path::new("/opt/homebrew/bin/git")));
        let full = StandIns { developer: true, java: true, xcode: None };
        assert!(
            full.answer(Path::new("/usr/bin/swift")) && full.answer(Path::new("/usr/bin/java"))
        );
    }

    /// Xcode's version is read from the plist beside its developer directory, and the command
    /// line tools alone, with no plist, have no Xcode.
    #[cfg(target_os = "macos")]
    #[test]
    fn xcode_is_read_from_its_plist_and_never_run() {
        let xcode = tempfile::tempdir().unwrap();
        let developer = xcode.path().join("Developer");
        std::fs::create_dir_all(&developer).unwrap();
        assert_eq!(xcode_version(&developer), None, "no plist, no Xcode");
        let plist = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                     <key>CFBundleShortVersionString</key><string>27.0</string>\
                     <key>ProductBuildVersion</key><string>27A266a</string></dict></plist>";
        std::fs::write(xcode.path().join("version.plist"), plist).unwrap();
        assert_eq!(xcode_version(&developer).as_deref(), Some("27.0"));
        assert!(TOOLCHAINS.iter().all(|tool| tool.program != "xcodebuild"));
    }

    #[test]
    fn the_power_source_and_the_gpus_read_from_their_tools() {
        let batt = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t80%;";
        assert_eq!(power_source(batt), Some("battery"));
        assert_eq!(power_source("Now drawing from 'AC Power'\n"), Some("ac"));
        assert_eq!(power_source("Now drawing from 'UPS Power'\n"), None);
        let smi = "GPU 0: NVIDIA A100-SXM4-40GB (UUID: GPU-1)\nGPU 1: NVIDIA L4 (UUID: GPU-2)\n";
        assert_eq!(nvidia_gpus(smi), ["NVIDIA A100-SXM4-40GB", "NVIDIA L4"]);
    }

    /// This Mac's own facts: its chip, its toolchains, and the maps always there, but nothing
    /// the server fills in itself.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn this_mac_reports_its_toolchains() {
        let facts = gather().await;
        assert!(
            matches!(facts.get("gpus"), Some(Fact::List(gpus)) if !gpus.is_empty()),
            "{facts:?}"
        );
        let Some(Fact::Map(toolchains)) = facts.get("toolchains") else { panic!("{facts:?}") };
        assert!(toolchains.contains_key("cargo"), "the test runs under cargo: {toolchains:?}");
        assert!(matches!(facts.get("agents"), Some(Fact::Map(_))));
        for server_filled in ["os", "os_version", "arch", "memory_mb", "encoders", "displays"] {
            assert!(!facts.contains_key(server_filled), "{server_filled}");
        }
    }

    /// A program that starts a child of its own, says the child's pid in `pidfile`, and waits
    /// on it: hung, as a command stuck on the network.
    fn hung(dir: &Path) -> (PathBuf, PathBuf) {
        let program = dir.join("hung");
        let pidfile = dir.join("child.pid");
        let script = format!(
            "#!/bin/sh\nsleep 600 &\necho $! > {}\nwait\n",
            slopty_core::shell_quote(&pidfile.to_string_lossy())
        );
        script_at(&program, &script);
        (program, pidfile)
    }

    fn script_at(path: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The pid a hung program wrote, once it has.
    async fn child_of(pidfile: &Path) -> rustix::process::Pid {
        let written = async {
            loop {
                let text = tokio::fs::read_to_string(pidfile).await.unwrap_or_default();
                if let Ok(pid) = text.trim().parse::<i32>() {
                    return rustix::process::Pid::from_raw(pid).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), written).await.expect("it started")
    }

    /// Whether `pid` is gone within a few seconds.
    async fn gone(pid: rustix::process::Pid) -> bool {
        for _ in 0..500 {
            if rustix::process::test_kill_process(pid).is_err() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// `git` in `dir`, with no config of the developer's, answering its output.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let git = crate::changes::git().expect("git");
        let out = std::process::Command::new(git)
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// The clones under `~/slopty/clones` are listed by the keys of their identity, each in
    /// the first path that has it; a clone under way (hidden beside its place) and a folder
    /// that holds no repository are not. A clone made later joins the list at once, and a
    /// gathering that started before it keeps it.
    #[tokio::test]
    async fn the_clones_made_for_the_server_are_a_fact() {
        if crate::changes::git().is_none() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("clones");
        let demo = root.join("example.com").join("o").join("demo");
        std::fs::create_dir_all(&demo).unwrap();
        git_in(&demo, &["init", "-q", "-b", "main"]);
        git_in(&demo, &["remote", "add", "origin", "https://example.com/o/demo.git"]);
        git_in(&demo, &["commit", "-q", "--allow-empty", "-m", "c0"]);
        let first = git_in(&demo, &["rev-parse", "HEAD"]);
        let partial = root.join("example.com").join("o").join(".other.partial");
        std::fs::create_dir_all(partial.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("example.com").join("empty")).unwrap();

        let Fact::Map(found) = clones(&root).await else { panic!("a map") };
        let at = Fact::Text(demo.to_string_lossy().into_owned());
        assert_eq!(found.get("example.com/o/demo"), Some(&at), "by its origin");
        assert_eq!(found.get(&first), Some(&at), "by its first commit");
        assert_eq!(found.len(), 2, "{found:?}");
        let Fact::Map(none) = clones(&tmp.path().join("nowhere")).await else { panic!() };
        assert!(none.is_empty());

        let (tx, rx) = watch::channel(Facts::new());
        let other = root.join("example.com").join("o").join("other");
        std::fs::create_dir_all(&other).unwrap();
        let id = slopty_proto::terminal::RepoId {
            origin: Some("example.com/o/other".to_owned()),
            root: None,
            url: None,
        };
        clone_made(&tx, &other, &id);
        let made = rx.borrow().get(REPOS).cloned();
        let want = Fact::Text(other.to_string_lossy().into_owned());
        let Some(Fact::Map(made)) = made else { panic!("{made:?}") };
        assert_eq!(made.get("example.com/o/other"), Some(&want));
        let mut gone = made.clone();
        gone.insert(
            "gone".to_owned(),
            Fact::Text(root.join("gone").to_string_lossy().into_owned()),
        );
        let kept: Facts = still_there(gone).into_iter().collect();
        assert_eq!(kept, made, "a clone whose folder went is let go");
    }

    /// A command that hangs past the wait is killed with everything it started. A machine so
    /// loaded that the program was killed before it could name its child is tried again with
    /// twice the wait, since that run shows nothing of the child.
    #[tokio::test]
    async fn a_hung_command_dies_with_its_children_after_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let (program, pidfile) = hung(dir.path());
        let mut wait = Duration::from_millis(1500);
        let child = loop {
            let waited = Instant::now();
            let command = tokio::process::Command::new(&program);
            let ran = run(command, wait, VERSION_OUTPUT_MAX).await;
            assert!(ran.is_none(), "no answer past the wait");
            assert!(waited.elapsed() < wait.saturating_add(Duration::from_secs(8)));
            let named = std::fs::read_to_string(&pidfile).unwrap_or_default();
            if let Ok(pid) = named.trim().parse::<i32>() {
                break rustix::process::Pid::from_raw(pid).unwrap();
            }
            assert!(wait < Duration::from_secs(20), "the program never named its child");
            wait = wait.saturating_mul(2);
        };
        assert!(gone(child).await, "its child outlived the wait");
    }

    /// A command the worker stops waiting on (it shuts down, the task is aborted) takes its
    /// whole group with it.
    #[tokio::test]
    async fn an_abandoned_command_dies_with_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let (program, pidfile) = hung(dir.path());
        let command = tokio::process::Command::new(&program);
        let polling = tokio::spawn(run(command, Duration::from_secs(600), VERSION_OUTPUT_MAX));
        let child = child_of(&pidfile).await;
        polling.abort();
        assert!(matches!(polling.await, Err(e) if e.is_cancelled()));
        assert!(gone(child).await, "its child outlived the poll");
    }

    /// A stand-in `claude` in `dir` that logs every word it is given and answers
    /// `--managed-help` as a managed launcher does (`managed`) or as Claude Code does.
    fn stand_in_claude(dir: &Path, managed: bool) -> (PathBuf, PathBuf) {
        let program = dir.join("claude");
        let log = dir.join("asked");
        let help = if managed {
            "echo 'Usage: claude managed <COMMAND>'; exit 0"
        } else {
            "echo \"error: unknown option '--managed-help'\" >&2; exit 1"
        };
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> {log}\nif [ \"$1\" = --managed-help ]; then {help}; fi\n\
             echo '2.1.300 (Claude Code)'\n",
            log = slopty_core::shell_quote(&log.to_string_lossy()),
        );
        script_at(&program, &script);
        (program, log)
    }

    fn asked(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    /// A managed launcher is asked only whether it is one, once, and its version is its newest
    /// client's; Claude Code itself is asked its version; a launcher's client is asked nothing,
    /// its version being in its path.
    #[tokio::test]
    async fn a_managed_launcher_is_never_asked_its_version() {
        let dir = tempfile::tempdir().unwrap();
        let search = SearchPath::of(None);
        let data = dir.path().join("share");
        let artifacts = data.join("claude-managed").join("artifacts");
        for version in ["2.1.9", "2.1.289", "2.1.30"] {
            std::fs::create_dir_all(artifacts.join(version).join("darwin-arm64")).unwrap();
        }

        let launcher_dir = dir.path().join("launcher");
        std::fs::create_dir_all(&launcher_dir).unwrap();
        let (launcher, log) = stand_in_claude(&launcher_dir, true);
        for _ in 0..2 {
            let version = claude_version(&search, &launcher, &data).await;
            assert_eq!(version.as_deref(), Some("2.1.289"));
        }
        assert_eq!(asked(&log), ["--managed-help"], "asked once, and nothing else");

        let plain_dir = dir.path().join("plain");
        std::fs::create_dir_all(&plain_dir).unwrap();
        let (plain, log) = stand_in_claude(&plain_dir, false);
        for _ in 0..2 {
            let version = claude_version(&search, &plain, &data).await;
            assert_eq!(version.as_deref(), Some("2.1.300"));
        }
        assert_eq!(asked(&log), ["--managed-help", "--version", "--version"]);

        let client_dir = artifacts.join("2.1.289").join("darwin-arm64");
        let (client, log) = stand_in_claude(&client_dir, false);
        let linked = dir.path().join("bin");
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(&client, linked.join("claude")).unwrap();
        let version = claude_version(&search, &linked.join("claude"), &data).await;
        assert_eq!(version.as_deref(), Some("2.1.289"));
        assert_eq!(asked(&log), Vec::<String>::new());
    }
}

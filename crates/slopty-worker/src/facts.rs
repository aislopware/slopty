//! What this worker has, as the [`Facts`] a project's placement reads.
//!
//! The agents and toolchains installed, the GPUs, the power source, and the person's own labels
//! and probes (`docs/decisions/projects.md`). The agents reached over ACP are under `acp`, named
//! as the ACP registry names them (`slopty_agent::acp::registry`), which is also how their
//! threads' agent is named (`acp:<name>`): what a client offers to start is what is there.
//!
//! The server fills in what it knows itself, from the worker's registration and capabilities
//! (`os`, `arch`, `cpus`, `memory_mb`, `encoders`, `displays`, `load` and the rest), so none of
//! those is here.
//!
//! None of this is on a path anyone waits on. [`watch()`] gathers on a task of its own, once the
//! daemon is up and every [`REFRESH`] after, and publishes a map only when it differs from the
//! last. Every command is bounded by a timeout, the independent ones run at once, and each runs
//! at utility priority in a process group of its own, which a timeout kills whole. A tool
//! that is missing or does not answer in time is only absent.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use slopty_agent::acp::registry;
use slopty_proto::project::{Fact, Facts};
use tokio::io::AsyncReadExt as _;
use tokio::sync::watch;
use tokio::task::JoinSet;

/// How often the facts are gathered again: installs and upgrades are rare, and a new label
/// waits at most this long.
pub const REFRESH: Duration = Duration::from_mins(10);
/// How long a person's probe may run.
pub const PROBE_WAIT: Duration = Duration::from_secs(5);
/// How much of a probe's output is kept, in bytes.
pub const PROBE_OUTPUT_MAX: usize = 1024;
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

/// What the person says of this machine, from `[worker.labels]`, `[worker.probes]` and
/// `[worker.acp]`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Own {
    /// Their labels, as facts.
    pub labels: Facts,
    /// Their probes: a name and the shell command whose answer it reports.
    pub probes: BTreeMap<String, String>,
    /// Their own ACP agents: a name and its command line ([`registry::registry`]).
    pub acp: BTreeMap<String, Vec<String>>,
}

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

/// The coding agents' command lines, under `agents`. Those reached over ACP are not listed here
/// but taken from the registry, under `acp` ([`acp_agents`]).
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

/// Keep `facts` current until nobody watches it: gathered now and every [`REFRESH`], with what
/// `own` reads of the person's settings then.
pub async fn watch<F>(facts: watch::Sender<Facts>, own: F)
where
    F: Fn() -> Own + Send + Sync + 'static,
{
    let own = Arc::new(own);
    let mut tick = tokio::time::interval(REFRESH);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if facts.is_closed() {
            return;
        }
        let read = Arc::clone(&own);
        let own_now = tokio::task::spawn_blocking(move || read()).await.unwrap_or_default();
        publish(&facts, gather(own_now).await);
    }
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
/// `agents`, `toolchains`, `labels` and `probes` are always there, empty when nothing is, so a
/// rule can ask what is in them.
pub async fn gather(own: Own) -> Facts {
    let (login, stand_ins) = tokio::join!(login_path(), StandIns::find());
    let search = Arc::new(SearchPath::of(std::env::var_os("PATH").into_iter().chain(login)));
    let acp = registry::registry(&own.acp);
    let (agents, acp, toolchains, rust_targets, gpus, power, probes) = tokio::join!(
        versions(&search, &stand_ins, &AGENTS),
        acp_agents(&search, &stand_ins, acp),
        versions(&search, &stand_ins, TOOLCHAINS),
        rust_targets(&search),
        gpus(&search),
        power(),
        probes(&search, own.probes, PROBE_WAIT),
    );
    let mut toolchains = toolchains;
    if let Some(xcode) = stand_ins.xcode {
        toolchains.insert("xcode".to_owned(), Fact::Text(xcode));
    }
    let mut facts = Facts::new();
    facts.insert("agents".to_owned(), Fact::Map(agents));
    facts.insert("acp".to_owned(), Fact::Map(acp));
    facts.insert("toolchains".to_owned(), Fact::Map(toolchains));
    facts.insert("labels".to_owned(), Fact::Map(own.labels));
    facts.insert("probes".to_owned(), Fact::Map(probes));
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

/// The versions of those of `tools` installed here, by name.
async fn versions(search: &Arc<SearchPath>, stand_ins: &StandIns, tools: &'static [Tool]) -> Facts {
    let mut running = JoinSet::new();
    for tool in tools {
        let Some(program) = search.resolve(tool.program).filter(|p| stand_ins.answer(p)) else {
            continue;
        };
        let search = Arc::clone(search);
        running.spawn(async move {
            let ran =
                run(search.command(&program, tool.args), VERSION_WAIT, VERSION_OUTPUT_MAX).await?;
            let version = ran.ok.then(|| version_in(&ran.out, tool.after)).flatten()?;
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

/// Those of the ACP `agents` installed here, by the registry's name: the version the program
/// says, or `true` when it says none (a program that serves ACP alone may take no `--version`).
async fn acp_agents(
    search: &Arc<SearchPath>,
    stand_ins: &StandIns,
    agents: Vec<registry::Agent>,
) -> Facts {
    let mut running = JoinSet::new();
    for agent in agents {
        let Some(program) = search.resolve(&agent.program).filter(|p| stand_ins.answer(p)) else {
            continue;
        };
        let search = Arc::clone(search);
        running.spawn(async move {
            let ran =
                run(search.command(&program, &["--version"]), VERSION_WAIT, VERSION_OUTPUT_MAX)
                    .await;
            let version = ran.filter(|ran| ran.ok).and_then(|ran| version_in(&ran.out, None));
            (agent.name, version.map_or(Fact::Bool(true), Fact::Text))
        });
    }
    let mut found = Facts::new();
    while let Some(done) = running.join_next().await {
        if let Ok((name, fact)) = done {
            found.insert(name, fact);
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

/// The Rust targets rustup has installed, when it is here.
async fn rust_targets(search: &SearchPath) -> Option<Vec<String>> {
    let rustup = search.resolve("rustup")?;
    let listed = search.command(&rustup, &["target", "list", "--installed"]);
    let ran = run(listed, VERSION_WAIT, LISTING_MAX).await.filter(|ran| ran.ok)?;
    Some(ran.out.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect())
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

/// Each of the person's probes run, by name.
async fn probes(
    search: &Arc<SearchPath>,
    probes: BTreeMap<String, String>,
    wait: Duration,
) -> Facts {
    let mut running = JoinSet::new();
    for (name, line) in probes {
        let search = Arc::clone(search);
        running.spawn(async move { (name, probe(&search, &line, wait).await) });
    }
    let mut answers = Facts::new();
    while let Some(done) = running.join_next().await {
        if let Ok((name, answer)) = done {
            answers.insert(name, answer);
        }
    }
    answers
}

/// What `line` answers through `sh -c` within `wait`: what it printed, trimmed and cut at
/// [`PROBE_OUTPUT_MAX`]; `true` when it succeeded silently; `false` when it failed or ran over.
async fn probe(search: &SearchPath, line: &str, wait: Duration) -> Fact {
    let shell = search.command(Path::new("/bin/sh"), &["-c", line]);
    match run(shell, wait, PROBE_OUTPUT_MAX).await {
        Some(Ran { ok: true, out }) if out.is_empty() => Fact::Bool(true),
        Some(Ran { ok: true, out }) => Fact::Text(out),
        Some(Ran { ok: false, .. }) | None => Fact::Bool(false),
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
    let ran = run(search.command(&found, &["--version"]), VERSION_WAIT, VERSION_OUTPUT_MAX).await;
    let version = ran.filter(|ran| ran.ok).and_then(|ran| version_in(&ran.out, None));
    Some(Installed { program: found, path: search.joined, version })
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
/// caller stops waiting (a worker shutting down drops it): a probe's shell dies with whatever
/// it started. Past the cap its output is read and dropped, so it finishes rather than
/// blocking on a full pipe.
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
    let mut group = Group(
        child.id().and_then(|id| i32::try_from(id).ok()).and_then(rustix::process::Pid::from_raw),
    );
    let mut stdout = child.stdout.take()?;
    let finished = tokio::time::timeout(wait, async {
        let mut kept = Vec::new();
        let limit = u64::try_from(cap).unwrap_or(u64::MAX);
        (&mut stdout).take(limit).read_to_end(&mut kept).await.ok()?;
        tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await.ok()?;
        let status = child.wait().await.ok()?;
        // Reaped: its id may name another group from now on.
        group.0 = None;
        Some(Ran { ok: status.success(), out: text_of(&kept) })
    })
    .await;
    finished.ok().flatten()
}

/// A child's process group, killed whole when dropped unless the child was reaped.
struct Group(Option<rustix::process::Pid>);

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            let _gone = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
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

    fn here() -> SearchPath {
        SearchPath::of(std::env::var_os("PATH"))
    }

    /// A probe says what it printed, `true` for silence, `false` for failure.
    #[tokio::test]
    async fn a_probe_answers_with_its_output_or_whether_it_succeeded() {
        let search = here();
        let answer = async |line: &str| probe(&search, line, PROBE_WAIT).await;
        assert_eq!(answer("printf '  ok\\n\\n'").await, Fact::Text("ok".to_owned()), "trimmed");
        assert_eq!(answer("true").await, Fact::Bool(true));
        assert_eq!(answer("printf said; exit 3").await, Fact::Bool(false), "it failed");
        assert_eq!(answer("no-such-program-anywhere").await, Fact::Bool(false));
        assert_eq!(answer("echo \"$PATH\" | grep -q /").await, Fact::Bool(true), "PATH is set");
    }

    /// A probe that runs over is `false` at its time, and what it started dies with it.
    #[tokio::test]
    async fn a_probe_past_its_time_is_false_and_leaves_nothing_running() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let line = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        let start = Instant::now();
        let answer = probe(&here(), &line, Duration::from_millis(500)).await;
        assert_eq!(answer, Fact::Bool(false));
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        let pid = rustix::process::Pid::from_raw(pid).unwrap();
        let gone = async {
            while rustix::process::test_kill_process(pid).is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), gone).await.expect("the sleep was killed");
    }

    /// A probe's output is cut at the cap, and the rest is drained so it still finishes.
    #[tokio::test]
    async fn a_long_output_is_cut_at_the_cap() {
        let line = "head -c 1000000 /dev/zero | tr '\\0' a";
        // The cap is under test, not the clock: a probe runs at utility priority, which a loaded
        // machine starves past `PROBE_WAIT`.
        let answer = probe(&here(), line, Duration::from_secs(120)).await;
        let Fact::Text(out) = answer else { panic!("a text, not {answer:?}") };
        assert_eq!(out.len(), PROBE_OUTPUT_MAX);
        assert!(out.bytes().all(|b| b == b'a'));
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

    /// The ACP agents installed are named as the registry names them, whatever their program
    /// is called: `cursor-agent` is `cursor`. One that says no version is there all the same, and
    /// the person's own is found where they put it.
    #[tokio::test]
    async fn the_acp_agents_installed_are_named_as_the_registry_names_them() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().canonicalize().unwrap();
        let mine = bin.join("mine").join("agent");
        std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
        let programs = [
            (bin.join("opencode"), "echo 1.18.34"),
            (bin.join("cursor-agent"), "exit 2"),
            (bin.join("amp"), "echo amp 0.9.1"),
            (mine.clone(), "echo mine 2.0"),
        ];
        for (path, body) in &programs {
            std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let own = BTreeMap::from([(
            "mine".to_owned(),
            vec![mine.to_string_lossy().into_owned(), "--stdio".to_owned()],
        )]);
        let search = Arc::new(SearchPath::of([bin.into_os_string()]));
        let stand_ins = StandIns { developer: true, java: true, xcode: None };
        let found = acp_agents(&search, &stand_ins, registry::registry(&own)).await;
        let want = Facts::from([
            ("cursor".to_owned(), Fact::Bool(true)),
            ("mine".to_owned(), Fact::Text("2.0".to_owned())),
            ("opencode".to_owned(), Fact::Text("1.18.34".to_owned())),
        ]);
        assert_eq!(found, want, "amp is not amp-acp, the program that serves ACP");
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
    async fn this_mac_reports_its_toolchains_and_what_its_person_said() {
        let own = Own {
            labels: Facts::from([("rack".to_owned(), Fact::Text("b2".to_owned()))]),
            probes: BTreeMap::from([("ok".to_owned(), "true".to_owned())]),
            acp: BTreeMap::new(),
        };
        let facts = gather(own).await;
        assert!(
            matches!(facts.get("gpus"), Some(Fact::List(gpus)) if !gpus.is_empty()),
            "{facts:?}"
        );
        let Some(Fact::Map(toolchains)) = facts.get("toolchains") else { panic!("{facts:?}") };
        assert!(toolchains.contains_key("cargo"), "the test runs under cargo: {toolchains:?}");
        assert!(matches!(facts.get("agents"), Some(Fact::Map(_))));
        let probes = Facts::from([("ok".to_owned(), Fact::Bool(true))]);
        assert_eq!(facts.get("probes"), Some(&Fact::Map(probes)));
        let rack = Facts::from([("rack".to_owned(), Fact::Text("b2".to_owned()))]);
        assert_eq!(facts.get("labels"), Some(&Fact::Map(rack)));
        for server_filled in ["os", "os_version", "arch", "memory_mb", "encoders", "displays"] {
            assert!(!facts.contains_key(server_filled), "{server_filled}");
        }
    }
}

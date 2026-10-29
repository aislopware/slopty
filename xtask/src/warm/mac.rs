//! `xtask warm mac`: the warm Mac runner, a launchd daemon under a macOS user of its own.
//!
//! The repository is public, so the runner holds none of the person's credentials: it runs as
//! [`USER`], a hidden account outside `staff` (whose group may read the person's home), with its
//! home on the build volume. That volume ignores ownership (`noowners`, as an external disk is
//! mounted), so every local account may read and write everything on it: the daemon runs inside
//! a sandbox ([`sandbox_profile`]) that denies it every volume path but its own, every home
//! under `/Users`, the person's files in the shared `/tmp`, and the person's services on
//! loopback. It runs at `Nice 10` with low-priority I/O, and keeps `ProcessType Standard`:
//! `Background` confines a job to the two efficiency cores, and a build there took 5.3 times as
//! long (`docs/decisions/tooling.md`, "The gate runs on warm runners here").
//!
//! Creating the user and loading the daemon need an administrator; `install` does everything
//! else and prints those commands.

use std::fmt::Write as _;

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

use super::{GUARD_CONFIG, GUARD_HOOK, MAC_LABEL};

/// The runner's macOS account.
pub const USER: &str = "slopty-ci";

/// Its user and group ID: below the IDs macOS gives people's accounts (501 up), hidden from the
/// login window besides.
const ID: u32 = 600;

/// The daemon of the Mac runner, and of the Linux runner's VM.
const DAEMON: &str = "com.slopty.warm-mac";
const VM_DAEMON: &str = "com.slopty.warm-linux";

/// Where both daemons write their output: the boot volume, readable when the build volume is
/// not mounted or the daemon may not reach it.
const LOGS: &str = "/Library/Logs/slopty-warm";

/// Cargo's jobs and nextest's test threads (`.config/nextest.toml`, `warm`): eight of the ten
/// cores, so the person and the lanes keep two at any load.
const JOBS: u32 = 8;

/// The macOS ephemeral port range starts here: a test's own server gets a port at or above it,
/// so a loopback port below it that the person serves is never one a test connects to.
const EPHEMERAL: u16 = 49_152;

/// Where the Mac runner keeps everything, under the warm runners' home.
#[derive(Debug)]
pub struct Layout {
    /// `<home>/mac`.
    pub root: Utf8PathBuf,
    /// The actions runner: its binaries, `.env`, `.path`, registration and `_work`.
    pub runner: Utf8PathBuf,
    /// [`USER`]'s home: rustup, cargo and every tool's cache.
    pub user_home: Utf8PathBuf,
    /// Its temporary directory, in place of the shared `/tmp`.
    pub tmp: Utf8PathBuf,
    /// The guard's binary, its config and the hook.
    pub guard: Utf8PathBuf,
    /// The runner and rustup installers.
    pub downloads: Utf8PathBuf,
}

impl Layout {
    pub fn of(home: &Utf8Path) -> Self {
        let root = home.join("mac");
        Self {
            runner: root.join("runner"),
            user_home: root.join("home"),
            tmp: root.join("tmp"),
            guard: root.join("guard"),
            downloads: root.join("downloads"),
            root,
        }
    }

    fn sandbox(&self) -> Utf8PathBuf {
        self.root.join("sandbox.sb")
    }

    fn plist(&self, label: &str) -> Utf8PathBuf {
        self.root.join(format!("{label}.plist"))
    }
}

/// `xtask warm mac install`.
pub fn install(sh: &Shell, home: &Utf8Path) -> Result<()> {
    anyhow::ensure!(cfg!(target_os = "macos"), "the warm Mac runner installs on a Mac");
    let layout = Layout::of(home);
    for dir in [&layout.runner, &layout.user_home, &layout.tmp, &layout.guard, &layout.downloads] {
        std::fs::create_dir_all(dir).with_context(|| format!("create {dir}"))?;
    }
    if !layout.runner.join("config.sh").is_file() {
        println!("▶ the actions runner {}", super::RUNNER_VERSION);
        let tarball = super::runner_tarball(sh, &layout.downloads, "osx-arm64")?;
        let runner = &layout.runner;
        cmd!(sh, "tar -xzf {tarball} -C {runner}").quiet().run()?;
    }
    let cargo_home = layout.user_home.join(".cargo");
    let rustup_home = layout.user_home.join(".rustup");
    if !cargo_home.join("bin").join("rustup").is_file() {
        println!("▶ rustup, for the runner's own toolchains");
        let init = super::rustup_init(sh, &layout.downloads, "aarch64-apple-darwin")?;
        cmd!(sh, "{init} -y --quiet --no-modify-path --profile minimal --default-toolchain none")
            .env("CARGO_HOME", &cargo_home)
            .env("RUSTUP_HOME", &rustup_home)
            .quiet()
            .ignore_stdout()
            .run()?;
    }
    install_guard(sh, &layout)?;
    let ports = person_s_ports(sh);
    write(&layout.sandbox(), &sandbox_profile(&layout, &ports))?;
    runner_environment(&layout)?;
    write(&layout.plist(DAEMON), &runner_plist(&layout))?;
    write(&layout.plist(VM_DAEMON), &vm_plist(home))?;
    println!("  ✓ the Mac runner's files are in {}", layout.root);
    if !layout.runner.join(".runner").is_file() {
        println!("  next: cargo xtask warm mac register");
    }
    let admin = admin_commands(sh, &layout);
    if admin.is_empty() {
        println!("  ✓ {USER} exists and both daemons are loaded");
    } else {
        println!("\nAn administrator runs, once:\n");
        for line in &admin {
            println!("  sudo {line}");
        }
    }
    Ok(())
}

/// This xtask as the guard, with the repository and its actors from GitHub, and the hook that
/// runs it. The sandbox keeps the runner's jobs from writing any of them.
fn install_guard(sh: &Shell, layout: &Layout) -> Result<()> {
    let exe = std::env::current_exe().context("this binary's path")?;
    let binary = layout.guard.join("xtask");
    let staged = layout.guard.join(".xtask.new");
    std::fs::copy(&exe, &staged).with_context(|| format!("copy {} to {staged}", exe.display()))?;
    std::fs::rename(&staged, &binary).with_context(|| format!("rename {staged} to {binary}"))?;
    let config = super::repository(sh)?;
    let config_file = layout.guard.join(GUARD_CONFIG);
    write(&config_file, &toml::to_string(&config)?)?;
    let hook = layout.guard.join(GUARD_HOOK);
    write(&hook, &super::hook_script(&binary, &config_file))?;
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    println!("  ✓ the guard: {} and the actors {}", config.repository, config.actors.join(", "));
    Ok(())
}

/// `contents` into `path`, replaced whole.
fn write(path: &Utf8Path, contents: &str) -> Result<()> {
    let staged = path.with_file_name(format!(".{}.new", path.file_name().unwrap_or("file")));
    std::fs::write(&staged, contents).with_context(|| format!("write {staged}"))?;
    std::fs::rename(&staged, path).with_context(|| format!("rename {staged} to {path}"))
}

/// The runner's `.env` and `.path`, which `config.sh` writes from its own environment: written
/// by `install` and again after `register`.
fn runner_environment(layout: &Layout) -> Result<()> {
    write(&layout.runner.join(".env"), &super::dotenv(&environment(layout)))?;
    write(&layout.runner.join(".path"), &format!("{}\n", path(layout)))
}

/// The runner's `.env`: its own homes, eight jobs, incremental builds, the hook, and the prune
/// budget of its target dir. The runner drops a variable whose value is empty, so the warm jobs
/// turn sccache off themselves (`RUSTC_WRAPPER: ""`): a warm target dir is the cache, and the
/// person's sccache server is not the runner's.
fn environment(layout: &Layout) -> Vec<(&'static str, String)> {
    vec![
        ("HOME", layout.user_home.to_string()),
        ("CARGO_HOME", layout.user_home.join(".cargo").to_string()),
        ("RUSTUP_HOME", layout.user_home.join(".rustup").to_string()),
        ("TMPDIR", format!("{}/", layout.tmp)),
        ("LANG", "en_US.UTF-8".to_owned()),
        ("CARGO_BUILD_JOBS", JOBS.to_string()),
        ("CARGO_INCREMENTAL", "1".to_owned()),
        ("SLOPTY_TARGET_BUDGET_GB", "120".to_owned()),
        ("ACTIONS_RUNNER_HOOK_JOB_STARTED", layout.guard.join(GUARD_HOOK).to_string()),
    ]
}

/// The runner's `PATH`: its own cargo, then Homebrew's tools (zig, git) and the system's.
fn path(layout: &Layout) -> String {
    format!(
        "{}:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        layout.user_home.join(".cargo").join("bin")
    )
}

/// The TCP ports below [`EPHEMERAL`] that the person's processes listen on, on any address:
/// their browser's debugging port, their compile cache, their tunnels. The sandbox denies the
/// runner each of them on loopback.
fn person_s_ports(sh: &Shell) -> Vec<u16> {
    let uid = rustix::process::getuid().as_raw().to_string();
    let listed = cmd!(sh, "lsof -nP -a -u {uid} -iTCP -sTCP:LISTEN -Fn")
        .quiet()
        .ignore_status()
        .ignore_stderr()
        .read()
        .unwrap_or_default();
    let mut ports: Vec<u16> = listed
        .lines()
        .filter_map(|line| line.strip_prefix('n')?.rsplit_once(':')?.1.parse().ok())
        .filter(|port| *port < EPHEMERAL)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// The sandbox the daemon runs the runner in. Everything is allowed but: reading or writing any
/// path under `/Volumes` or `/Users`, or the person's files in `/tmp`; then the runner's own
/// directory is allowed back, less the guard, the sandbox itself and the runner's `.env` and
/// `.path`, which no job may change; and connecting to `ports` on loopback. The directories
/// above the runner's own may be listed, as finding the working directory needs (the names of
/// the volumes and of the build volume's top folders, nothing in them). The last rule that
/// matches decides.
fn sandbox_profile(layout: &Layout, ports: &[u16]) -> String {
    let root = &layout.root;
    let above = root.ancestors().skip(1).filter(|dir| dir.parent().is_some()).fold(
        String::new(),
        |mut above, dir| {
            let _written = write!(above, " (literal \"{dir}\")");
            above
        },
    );
    let mut profile = format!(
        "(version 1)\n\
         (allow default)\n\
         ; Nothing of the person's: no volume path, no home, none of their files in /tmp.\n\
         (deny file-read-data file-write* (subpath \"/Volumes\") (subpath \"/Users\")\n\
         \x20   (regex #\"^/private/tmp/.\"))\n\
         ; The directories above the runner's own, which getcwd reads; then its own, its\n\
         ; temporary one included.\n\
         (allow file-read-data{above})\n\
         (allow file-read* file-write* (subpath \"{root}\"))\n\
         ; What no job may change: the guard, this profile, the runner's environment.\n\
         (deny file-write* (subpath \"{guard}\") (literal \"{sandbox}\")\n\
         \x20   (literal \"{env}\") (literal \"{path}\"))\n",
        guard = layout.guard,
        sandbox = layout.sandbox(),
        env = layout.runner.join(".env"),
        path = layout.runner.join(".path"),
    );
    if !ports.is_empty() {
        profile.push_str("; The person's services on loopback.\n(deny network-outbound");
        for port in ports {
            let _written = write!(profile, "\n    (remote tcp \"localhost:{port}\")");
        }
        profile.push_str(")\n");
    }
    profile
}

/// launchd's job for the runner: as [`USER`], in the sandbox, kept alive while the build volume
/// holds the runner, at `Nice 10` with low-priority I/O, with the descriptors a build needs
/// (launchd gives a job 256).
fn runner_plist(layout: &Layout) -> String {
    let run = layout.runner.join("run.sh");
    let args = [
        "/usr/bin/sandbox-exec".to_owned(),
        "-f".to_owned(),
        layout.sandbox().to_string(),
        run.to_string(),
    ];
    let env = [("HOME", layout.user_home.to_string()), ("TMPDIR", format!("{}/", layout.tmp))];
    plist(DAEMON, &args, &env, &layout.runner, &run, "mac")
}

/// launchd's job for the Linux runner's VM: the guard's `xtask warm linux run` as [`USER`],
/// which runs tart headless and lowers the VM's own process, kept alive while the build volume
/// holds the VM, at `Nice 10` with low-priority I/O.
fn vm_plist(home: &Utf8Path) -> String {
    let tart_home = super::linux::tart_home(home);
    let config = tart_home.join("vms").join(super::LINUX_LABEL).join("config.json");
    let args = [
        Layout::of(home).guard.join("xtask").to_string(),
        "warm".to_owned(),
        "linux".to_owned(),
        "run".to_owned(),
    ];
    let env = [
        ("HOME", home.join("linux").to_string()),
        ("SLOPTY_WARM_HOME", home.to_string()),
        ("PATH", "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_owned()),
    ];
    plist(VM_DAEMON, &args, &env, &home.join("linux"), &config, "linux")
}

/// A launchd daemon's property list.
fn plist(
    label: &str,
    args: &[String],
    env: &[(&str, String)],
    dir: &Utf8Path,
    alive_while: &Utf8Path,
    log: &str,
) -> String {
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    let mut key = |k: &str, v: &str| {
        let _written = writeln!(text, "  <key>{k}</key>{v}");
    };
    key("Label", &format!("<string>{label}</string>"));
    key("UserName", &format!("<string>{USER}</string>"));
    key("GroupName", &format!("<string>{USER}</string>"));
    let args = args.iter().fold(String::new(), |mut out, a| {
        let _written = write!(out, "<string>{}</string>", escape(a));
        out
    });
    key("ProgramArguments", &format!("<array>{args}</array>"));
    let env = env.iter().fold(String::new(), |mut out, (k, v)| {
        let _written = write!(out, "<key>{k}</key><string>{}</string>", escape(v));
        out
    });
    key("EnvironmentVariables", &format!("<dict>{env}</dict>"));
    key("WorkingDirectory", &format!("<string>{}</string>", escape(dir.as_str())));
    key(
        "KeepAlive",
        &format!(
            "<dict><key>PathState</key><dict><key>{}</key><true/></dict></dict>",
            escape(alive_while.as_str())
        ),
    );
    key("ThrottleInterval", "<integer>30</integer>");
    key("Nice", "<integer>10</integer>");
    key("LowPriorityIO", "<true/>");
    key("ProcessType", "<string>Standard</string>");
    let files = "<dict><key>NumberOfFiles</key><integer>65536</integer></dict>";
    key("SoftResourceLimits", files);
    key("HardResourceLimits", files);
    key("StandardOutPath", &format!("<string>{LOGS}/{log}.log</string>"));
    key("StandardErrorPath", &format!("<string>{LOGS}/{log}.log</string>"));
    text.push_str("</dict>\n</plist>\n");
    text
}

/// `text` as XML character data.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// What an administrator still has to run, each line after `sudo`: the user and its group, the
/// log directory, and each daemon installed and loaded. Empty once all of it is done.
fn admin_commands(sh: &Shell, layout: &Layout) -> Vec<String> {
    let mut lines = Vec::new();
    let user_exists = cmd!(sh, "dscl . -read /Users/{USER} UniqueID")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_ok();
    if !user_exists {
        lines.extend(create_user(&layout.user_home));
    }
    if !Utf8Path::new(LOGS).is_dir() {
        lines.push(format!("mkdir -p {LOGS}"));
        lines.push(format!("chown {USER}:{USER} {LOGS}"));
    }
    for label in [DAEMON, VM_DAEMON] {
        let installed = Utf8PathBuf::from(format!("/Library/LaunchDaemons/{label}.plist"));
        let ours = std::fs::read_to_string(layout.plist(label)).unwrap_or_default();
        if std::fs::read_to_string(&installed).ok().as_deref() != Some(ours.as_str()) {
            let plist = layout.plist(label);
            if installed.is_file() {
                lines.push(format!("launchctl bootout system/{label}"));
            }
            lines.push(format!("install -m 644 -o root -g wheel {plist} {installed}"));
            lines.push(format!("launchctl bootstrap system {installed}"));
        }
    }
    lines
}

/// The `dscl` lines that make [`USER`]: a group of its own, so it is outside `staff`; no
/// password and no shell, so nobody logs in as it; hidden; its home on the build volume.
fn create_user(user_home: &Utf8Path) -> Vec<String> {
    let id = ID.to_string();
    [
        format!("dscl . -create /Groups/{USER} PrimaryGroupID {id}"),
        format!("dscl . -create /Groups/{USER} RealName 'Slopty CI'"),
        format!("dscl . -create /Users/{USER} UniqueID {id}"),
        format!("dscl . -create /Users/{USER} PrimaryGroupID {id}"),
        format!("dscl . -create /Users/{USER} RealName 'Slopty CI runner'"),
        format!("dscl . -create /Users/{USER} NFSHomeDirectory {user_home}"),
        format!("dscl . -create /Users/{USER} UserShell /usr/bin/false"),
        format!("dscl . -create /Users/{USER} Password '*'"),
        format!("dscl . -create /Users/{USER} IsHidden 1"),
    ]
    .into()
}

/// `xtask warm mac register`: the runner registered as [`MAC_LABEL`] with its labels, the
/// token passed in the environment `config.sh` reads, never on its command line.
pub fn register(sh: &Shell, home: &Utf8Path) -> Result<()> {
    let layout = Layout::of(home);
    anyhow::ensure!(
        layout.runner.join("config.sh").is_file(),
        "no runner in {}: `cargo xtask warm mac install` first",
        layout.runner
    );
    let config = super::repository(sh)?;
    let token = super::registration_token(sh, &config.repository)?;
    let url = format!("https://github.com/{}", config.repository);
    let runner = Shell::new()?;
    runner.change_dir(&layout.runner);
    cmd!(
        runner,
        "./config.sh --unattended --replace --url {url} --name {MAC_LABEL} --labels {MAC_LABEL} --work _work"
    )
    .env("ACTIONS_RUNNER_INPUT_TOKEN", token)
    .quiet()
    .run()
    .context("register the Mac runner")?;
    runner_environment(&layout)?;
    println!("  ✓ {MAC_LABEL} registered with {}", config.repository);
    Ok(())
}

/// The Mac daemon's state as launchd reports it, and the tail of its log.
pub fn status(sh: &Shell, home: &Utf8Path) {
    let layout = Layout::of(home);
    let state =
        cmd!(sh, "launchctl print system/{DAEMON}").quiet().ignore_stderr().read().ok().and_then(
            |out| out.lines().find_map(|l| l.trim().strip_prefix("state = ").map(str::to_owned)),
        );
    println!("▶ the Mac runner ({})", layout.root);
    println!("  daemon: {}", state.as_deref().unwrap_or("not loaded"));
    let registered = layout.runner.join(".runner").is_file();
    println!("  registered here: {}", if registered { "yes" } else { "no" });
    let log = Utf8PathBuf::from(format!("{LOGS}/mac.log"));
    if let Ok(text) = std::fs::read_to_string(&log) {
        let lines: Vec<&str> = text.lines().collect();
        for line in lines.iter().skip(lines.len().saturating_sub(3)) {
            println!("  log: {line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use super::{Layout, create_user, environment, plist, runner_plist, sandbox_profile, vm_plist};

    fn layout() -> Layout {
        Layout::of(Utf8Path::new("/Volumes/Build/slopty-ci"))
    }

    /// The sandbox denies the person's volumes, homes, `/tmp` files and services, gives the
    /// runner its own directory back, and keeps the guard and the runner's environment out of
    /// its jobs' reach, in that order, since the last rule that matches decides.
    #[test]
    fn the_sandbox_denies_the_person_s_files_and_services_and_the_guard() {
        let profile = sandbox_profile(&layout(), &[4226, 9333]);
        let at = |needle: &str| profile.find(needle).unwrap_or_else(|| panic!("{needle}"));
        let denied =
            at("(deny file-read-data file-write* (subpath \"/Volumes\") (subpath \"/Users\")");
        let tmp = at("(regex #\"^/private/tmp/.\")");
        let above = at("(allow file-read-data (literal \"/Volumes/Build/slopty-ci\") \
             (literal \"/Volumes/Build\") (literal \"/Volumes\"))");
        let own = at("(allow file-read* file-write* (subpath \"/Volumes/Build/slopty-ci/mac\"))");
        let guard = at("(deny file-write* (subpath \"/Volumes/Build/slopty-ci/mac/guard\")");
        assert!(denied < tmp && tmp < above && above < own && own < guard, "{profile}");
        assert!(profile.contains("(literal \"/Volumes/Build/slopty-ci/mac/runner/.env\")"));
        assert!(profile.contains("(remote tcp \"localhost:9333\")"), "{profile}");
        assert!(profile.contains("(remote tcp \"localhost:4226\")"), "{profile}");
        assert!(!sandbox_profile(&layout(), &[]).contains("network-outbound"));
    }

    /// The runner's environment names its own homes and temporary directory, turns incremental
    /// builds on, caps the jobs, sets the hook, and holds no empty value, which the runner drops.
    #[test]
    fn the_runner_s_environment_is_its_own() {
        let env = environment(&layout());
        let get = |key: &str| env.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str());
        assert_eq!(get("HOME"), Some("/Volumes/Build/slopty-ci/mac/home"));
        assert_eq!(get("CARGO_HOME"), Some("/Volumes/Build/slopty-ci/mac/home/.cargo"));
        assert_eq!(get("TMPDIR"), Some("/Volumes/Build/slopty-ci/mac/tmp/"));
        assert_eq!(get("CARGO_INCREMENTAL"), Some("1"));
        assert_eq!(get("CARGO_BUILD_JOBS"), Some("8"));
        assert!(env.iter().all(|(_, v)| !v.is_empty()), "{env:?}");
        assert_eq!(
            get("ACTIONS_RUNNER_HOOK_JOB_STARTED"),
            Some("/Volumes/Build/slopty-ci/mac/guard/started.sh")
        );
    }

    /// The daemons run the runner in the sandbox and the VM through the guard's xtask, as the
    /// runner's user, at `Nice 10` with low-priority I/O and `Standard`, never `Background`,
    /// and escape what XML would read.
    #[test]
    fn the_daemon_runs_sandboxed_niced_and_on_every_core() {
        let text = runner_plist(&layout());
        for needle in [
            "<key>UserName</key><string>slopty-ci</string>",
            "<string>/usr/bin/sandbox-exec</string><string>-f</string>\
             <string>/Volumes/Build/slopty-ci/mac/sandbox.sb</string>",
            "<key>Nice</key><integer>10</integer>",
            "<key>LowPriorityIO</key><true/>",
            "<key>ProcessType</key><string>Standard</string>",
            "<key>PathState</key><dict><key>/Volumes/Build/slopty-ci/mac/runner/run.sh</key>",
            "<key>NumberOfFiles</key><integer>65536</integer>",
        ] {
            assert!(text.contains(needle), "{needle} in {text}");
        }
        let vm = vm_plist(Utf8Path::new("/Volumes/Build/slopty-ci"));
        for needle in [
            "<key>UserName</key><string>slopty-ci</string>",
            "<string>/Volumes/Build/slopty-ci/mac/guard/xtask</string><string>warm</string>\
             <string>linux</string><string>run</string>",
            "<key>SLOPTY_WARM_HOME</key><string>/Volumes/Build/slopty-ci</string>",
            "<key>Nice</key><integer>10</integer>",
            "<key>PathState</key><dict>\
             <key>/Volumes/Build/slopty-ci/linux/tart/vms/slopty-warm-linux/config.json</key>",
        ] {
            assert!(vm.contains(needle), "{needle} in {vm}");
        }
        let odd =
            plist("x", &["a&b".to_owned()], &[], Utf8Path::new("/d"), Utf8Path::new("/k"), "l");
        assert!(odd.contains("<string>a&amp;b</string>"), "{odd}");
    }

    /// The user is made outside `staff`, with no password or shell, hidden, its home on the
    /// build volume.
    #[test]
    fn the_user_has_a_group_of_its_own_and_no_login() {
        let lines = create_user(Utf8Path::new("/Volumes/Build/slopty-ci/mac/home"));
        for needle in [
            "dscl . -create /Groups/slopty-ci PrimaryGroupID 600",
            "dscl . -create /Users/slopty-ci PrimaryGroupID 600",
            "dscl . -create /Users/slopty-ci UserShell /usr/bin/false",
            "dscl . -create /Users/slopty-ci Password '*'",
            "dscl . -create /Users/slopty-ci IsHidden 1",
            "dscl . -create /Users/slopty-ci NFSHomeDirectory /Volumes/Build/slopty-ci/mac/home",
        ] {
            assert!(lines.iter().any(|l| l == needle), "{needle}");
        }
        assert!(!lines.iter().any(|l| l.contains(" 20")), "never staff's group 20");
    }
}

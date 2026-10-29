//! `xtask warm mac`: the warm Mac runner, a launchd daemon under a macOS user of its own.
//!
//! The repository is public, so the runner holds none of the person's credentials: it runs as
//! [`USER`], a hidden account outside `staff` (whose group may read the person's home), with its
//! home on the startup disk ([`USER_HOME`]), where ownership holds, and everything large on the
//! warm runners' volume. That volume ignores ownership, as the external disk its image is on
//! does, so every local account may read and write what is on either, the person's checkouts and
//! the binaries they run among it: the daemon runs inside a sandbox ([`sandbox_profile`]) that
//! denies it every volume path and every home but its own, the person's files and sockets in the
//! shared `/tmp`, and the person's services on loopback. It runs at `Nice 10` with low-priority
//! I/O, and keeps `ProcessType Standard`: `Background` confines a job to the two efficiency
//! cores, and a build there took 5.3 times as long (`docs/decisions/tooling.md`, "The gate runs
//! on warm runners here").
//!
//! Creating the user and loading the daemons need an administrator: `install` does everything
//! else and writes what is left into one script ([`admin_script`]), which holds the daemons'
//! property lists itself and is written into the checkout, off the volume the runner's jobs
//! write, so root never runs what a job could have changed. `try` runs the same runner, in the
//! same sandbox, as the person, until the daemon takes over.

use std::fmt::Write as _;

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

use super::{GUARD_CONFIG, GUARD_HOOK, MAC_LABEL};

/// The runner's macOS account.
pub const USER: &str = "slopty-ci";

/// Its user and group ID, hidden from the login window besides.
const ID: u32 = 600;

/// Its home: on the startup disk, where ownership holds, so nobody else may write it, and where
/// the frameworks the tests use (`WebKit`, the Trash, a test of the startup disk) expect a home.
/// Its toolchains, caches and builds are on the warm runners' volume.
pub const USER_HOME: &str = "/Users/slopty-ci";

/// Its temporary directory, in its home in place of the shared `/tmp`: on the startup disk,
/// whose `FSEvents` keep the history the file-watching tests replay, which the external disk's
/// do not log (`.fseventsd/no_log`).
const USER_TMP: &str = "/Users/slopty-ci/tmp";

/// The daemon that attaches the warm runners' volume, [`USER`]'s pasteboard server, the Mac
/// runner's, and the Linux runner's VM's.
const VOLUME_DAEMON: &str = "com.slopty.warm-volume";
const PBOARD_DAEMON: &str = "com.slopty.warm-pboard";
const DAEMON: &str = "com.slopty.warm-mac";
pub const VM_DAEMON: &str = "com.slopty.warm-linux";

/// macOS's pasteboard server and the service AppKit looks it up by. launchd starts it in a login
/// session only, which [`USER`] never has; the daemon's own in the system domain, where the
/// runner's processes look, serves [`USER`] alone (`docs/decisions/tooling.md`, "The gate runs
/// on warm runners here").
const PBOARD: &str = "/usr/libexec/pboard";
const PASTEBOARD_SERVICE: &str = "com.apple.pasteboard.1";

/// The agent that runs the Mac runner as the person, before the daemon does (`try`).
pub const TRY_AGENT: &str = "com.slopty.warm-mac.try";
/// `try`'s home and temporary directory, in the person's temporary directory: on the startup
/// disk, as the tests that read `HOME` expect of a Mac's, and apart from the person's own files
/// and sockets there. The temporary directory's name is short, as a Unix socket's path is at
/// most 104 bytes and a test's tmux socket in it needs 93 of them.
const TRY_HOME: &str = "slopty-warm-home";
const TRY_TMP: &str = "warm";

/// Where the daemons write their output: the startup disk, readable when the warm runners'
/// volume is not attached.
const LOGS: &str = "/Library/Logs/slopty-warm";

/// Cargo's jobs and nextest's test threads (`.config/nextest.toml`, `warm`): eight of the ten
/// cores, so the person and the lanes keep two at any load.
const JOBS: u32 = 8;

/// The e2e harness's roots, in `/tmp` on a Mac (`slopty_e2e::harness`), as the sandbox matches
/// them.
const E2E_ROOTS: &str = "(regex #\"^/private/tmp/slopty-e2e-\")";

/// The macOS ephemeral port range starts here: a test's own server gets a port at or above it,
/// so a loopback port below it that the person serves is never one a test connects to.
const EPHEMERAL: u16 = 49_152;

/// The set-user-ID programs the tests start: `ps`, which several read process states with, and
/// `login`, which one starts on a terminal. A sandboxed process may not start a set-user-ID
/// program at all, so these alone start outside the sandbox. Neither gives anything without a
/// password, and the runner's account has none.
const SETUID: [&str; 2] = ["/bin/ps", "/usr/bin/login"];

/// The sockets in `/var/run` a job may connect to: the resolver's and the system log's. Every
/// other one there is the Mac's or the person's: launchd's for each login session, the person's
/// SSH agent among them, Docker's, the iPhone multiplexer's (which hands out pairing records),
/// the system keychain's check.
const RUN_SOCKETS: [&str; 2] = ["/private/var/run/mDNSResponder", "/private/var/run/syslog"];

/// The Mach services that hold or hand out a credential, which no job may look up: the
/// keychains' (the data-protection keychain's securityd, the system keychain's, and the agents
/// that unlock and sync them), authorization, smart cards' and tokens', the Apple account's and
/// every other account's, passkeys and saved passwords, Kerberos's, Touch ID's and the recovery
/// services'. Certificate trust (`trustd`), which TLS needs, and the checks of programs
/// (`syspolicyd`, `XProtect`) stay, and so does securityd's older service
/// (`com.apple.SecurityServer`): the runner is .NET, whose TLS reads each certificate through it
/// (without it, every connection failed with "bad certificate format"). The keychains it serves
/// are files, which the sandbox denies in every home, and a System keychain item's secret needs
/// an administrator's authorization, whose services are denied.
const CREDENTIAL_SERVICES: [&str; 12] = [
    "com.apple.systemkeychain",
    "com.apple.authd",
    "com.apple.keychainsharingmessagingd",
    "com.apple.cdp.daemon",
    "com.apple.cdpd",
    "com.apple.swcd",
    "com.apple.SharedWebCredentials",
    "com.apple.AppleCredentialManagerDaemon",
    "com.apple.TrustedPeersHelper",
    "com.apple.bird.token",
    "com.apple.gssd",
    "com.apple.security.XPCKeychainSandboxCheck",
];

/// The families of [`CREDENTIAL_SERVICES`], by the prefix each name starts with.
const CREDENTIAL_SERVICE_NAMES: [&str; 6] = [
    r"^com\.apple\.securityd(\.|$)",
    r"^com\.apple\.security\.(agent|authhost|authtrampoline|cloudkeychainproxy|escrow-update)",
    r"^com\.apple\.security\.(kcsharing|keychain-circle-notification|KeychainStasher|octagon)",
    r"^com\.apple\.(ctkd|CryptoTokenKit|ak|accountsd|amsaccountsd|aps\.amsaccountsd)(\.|$)",
    r"^com\.apple\.(AuthenticationServices(Core)?|identityservicesd|Kerberos)(\.|$)",
    r"^com\.apple\.(LocalAuthentication|passd|lskdd|SecureBackupDaemon|xpc\.amsaccountsd)(\.|$)",
];

/// Where the Mac runner keeps everything, on the warm runners' volume.
#[derive(Debug)]
pub struct Layout {
    /// `<home>/mac`.
    pub root: Utf8PathBuf,
    /// The actions runner: its binaries, `.env`, registration and `_work`.
    pub runner: Utf8PathBuf,
    /// rustup's and cargo's homes: the toolchains, the registry and the tools.
    pub rustup_home: Utf8PathBuf,
    pub cargo_home: Utf8PathBuf,
    /// The tools' caches (`XDG_CACHE_HOME`: zig's among them), off the startup disk.
    pub cache: Utf8PathBuf,
    /// The guard's binary, its config and the hook.
    pub guard: Utf8PathBuf,
    /// The runner and rustup installers.
    pub downloads: Utf8PathBuf,
    /// The volume's Trashes, where a file of the runner's goes when a test trashes it.
    pub trash: Utf8PathBuf,
}

impl Layout {
    /// The layout under the warm runners' `home`, the root of their volume.
    pub fn of(home: &Utf8Path) -> Self {
        let root = home.join("mac");
        Self {
            runner: root.join("runner"),
            rustup_home: root.join("rustup"),
            cargo_home: root.join("cargo"),
            cache: root.join("cache"),
            guard: root.join("guard"),
            downloads: root.join("downloads"),
            trash: home.join(".Trashes"),
            root,
        }
    }

    fn sandbox(&self) -> Utf8PathBuf {
        self.root.join("sandbox.sb")
    }

    /// `try`'s sandbox: the daemon's, and none of the person's sockets in their own temporary
    /// and cache directories ([`try_profile`]).
    fn try_sandbox(&self) -> Utf8PathBuf {
        self.root.join("try.sb")
    }
}

/// `xtask warm mac install`: the runner's files on the volume at `home`, attached from `image`,
/// and the administrator's script at `admin`.
pub fn install(sh: &Shell, home: &Utf8Path, image: &Utf8Path, admin: &Utf8Path) -> Result<()> {
    anyhow::ensure!(cfg!(target_os = "macos"), "the warm Mac runner installs on a Mac");
    let layout = Layout::of(home);
    for dir in [
        &layout.runner,
        &layout.rustup_home,
        &layout.cargo_home,
        &layout.cache,
        &layout.guard,
        &layout.downloads,
    ] {
        std::fs::create_dir_all(dir).with_context(|| format!("create {dir}"))?;
    }
    if !layout.runner.join("config.sh").is_file() {
        println!("▶ the actions runner {}", super::RUNNER_VERSION);
        let tarball = super::runner_tarball(sh, &layout.downloads, "osx-arm64")?;
        let runner = &layout.runner;
        cmd!(sh, "tar -xzf {tarball} -C {runner}").quiet().run()?;
    }
    if !layout.cargo_home.join("bin").join("rustup").is_file() {
        println!("▶ rustup, for the runner's own toolchains");
        let init = super::rustup_init(sh, &layout.downloads, "aarch64-apple-darwin")?;
        cmd!(sh, "{init} -y --quiet --no-modify-path --profile minimal --default-toolchain none")
            .env("CARGO_HOME", &layout.cargo_home)
            .env("RUSTUP_HOME", &layout.rustup_home)
            .quiet()
            .ignore_stdout()
            .run()?;
    }
    let ports = person_s_ports(sh);
    write(&layout.sandbox(), &sandbox_profile(&layout, &ports))?;
    runner_environment(&layout, &super::mount_point(image)?)?;
    let daemons = daemons(home, &layout, image)?;
    if let Some(dir) = admin.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {dir}"))?;
    }
    write(admin, &admin_script(&daemons, admin))?;
    // Last, as it needs `gh`: until it is there, the hook it writes is missing, and the runner
    // fails every job.
    install_guard(sh, &layout, &super::wake_file(home))?;
    println!("  ✓ the Mac runner's files are in {}", layout.root);
    if !runner_alive(&layout).is_file() {
        println!("  next: cargo xtask warm mac register");
    }
    if admin_done(&daemons) {
        println!("  ✓ {USER} exists and the daemons are installed as written");
    } else {
        println!("\nAn administrator runs, once and after every install:\n");
        println!("  sudo sh {admin}");
    }
    Ok(())
}

/// This xtask as the guard, with the repository and its actors from GitHub, and the hook that
/// runs it and then touches `wake`, which boots the Linux runner's VM. The sandbox keeps the
/// runner's jobs from writing any of them but `wake`.
fn install_guard(sh: &Shell, layout: &Layout, wake: &Utf8Path) -> Result<()> {
    let exe = std::env::current_exe().context("this binary's path")?;
    let binary = layout.guard.join("xtask");
    let staged = layout.guard.join(".xtask.new");
    std::fs::copy(&exe, &staged).with_context(|| format!("copy {} to {staged}", exe.display()))?;
    std::fs::rename(&staged, &binary).with_context(|| format!("rename {staged} to {binary}"))?;
    let config = super::repository(sh)?;
    let config_file = layout.guard.join(GUARD_CONFIG);
    write(&config_file, &toml::to_string(&config)?)?;
    let hook = layout.guard.join(GUARD_HOOK);
    write(&hook, &super::hook_script(&binary, &config_file, Some(wake)))?;
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    println!("  ✓ the guard: {} and the actors {}", config.repository, config.actors.join(", "));
    Ok(())
}

/// `contents` into `path`, replaced whole.
pub fn write(path: &Utf8Path, contents: &str) -> Result<()> {
    let staged = path.with_file_name(format!(".{}.new", path.file_name().unwrap_or("file")));
    std::fs::write(&staged, contents).with_context(|| format!("write {staged}"))?;
    std::fs::rename(&staged, path).with_context(|| format!("rename {staged} to {path}"))
}

/// The runner's `.env`, which `config.sh` writes from its own environment: written by `install`
/// and again after `register`. Its `.path` is read only by the runner's own service script,
/// which launchd's jobs do not run: `PATH` is each property list's ([`path`]). `disk` is the
/// disk the volume's image is on.
fn runner_environment(layout: &Layout, disk: &Utf8Path) -> Result<()> {
    write(&layout.runner.join(".env"), &super::dotenv(&environment(layout, disk)))
}

/// The runner's `.env`: its toolchains and caches, eight jobs, incremental builds, the hook,
/// what its target dir may hold and the volume keep free, which leave the Linux VM's disk its
/// share of the volume, and the person's `disk` the volume's image grows into, which a run keeps
/// that floor free on too (`crate::gate::warm::DISK_VAR`). `HOME`, `TMPDIR` and `PATH` are the
/// daemon's or `try`'s (each property list's), which the runner hands its jobs. The runner drops a
/// variable whose value is empty, so the warm jobs turn sccache off themselves (`RUSTC_WRAPPER:
/// ""`): a warm target dir is the cache, and the person's sccache server is not the runner's.
fn environment(layout: &Layout, disk: &Utf8Path) -> Vec<(&'static str, String)> {
    vec![
        ("CARGO_HOME", layout.cargo_home.to_string()),
        ("RUSTUP_HOME", layout.rustup_home.to_string()),
        ("XDG_CACHE_HOME", layout.cache.to_string()),
        ("LANG", "en_US.UTF-8".to_owned()),
        ("CARGO_BUILD_JOBS", JOBS.to_string()),
        ("CARGO_INCREMENTAL", "1".to_owned()),
        ("SLOPTY_TARGET_BUDGET_GB", "70".to_owned()),
        ("SLOPTY_DISK_FLOOR_GB", "15".to_owned()),
        (crate::gate::warm::DISK_VAR, disk.to_string()),
        ("ACTIONS_RUNNER_HOOK_JOB_STARTED", layout.guard.join(GUARD_HOOK).to_string()),
    ]
}

/// The runner's `PATH`: its own cargo, then Homebrew's tools (git, gh) and the system's.
fn path(layout: &Layout) -> String {
    format!("{}:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin", layout.cargo_home.join("bin"))
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
/// directory, its home, the volume's Trashes and the e2e harness's roots in `/tmp` (one path on
/// every Mac, so that renders match) are allowed back, less the guard, the sandboxes
/// themselves and the runner's `.env`, which no job may change, nor move the directories
/// holding them away to remake them; connecting to a socket in a home, on a volume or in `/tmp`
/// but the runner's own, the person's agents among them, to one in `/var/run` but
/// [`RUN_SOCKETS`], where launchd keeps each login session's SSH agent, or to `ports` on
/// loopback; looking up a Mach service that holds a credential, the keychains' first
/// ([`CREDENTIAL_SERVICES`]); and starting a set-user-ID program but [`SETUID`]'s, which start
/// outside it. The directories above the runner's own may be listed, as finding the working
/// directory needs (the names of the volumes and of the volume's top folders, nothing in them).
/// A hard link to a file it may not write is denied too.
///
/// Of the rules that name an operation, the last that matches decides; a rule naming a wider one
/// (`file-read*`) does not undo one naming `file-read-data`, so every allow names its deny's
/// operations. What the sandbox costs: a process in it may not apply a sandbox of its own, so
/// `sshd`, which sandboxes its pre-authentication child, does not run in it, nor do the tests
/// that start one (`.config/nextest.toml`, `warm`).
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
         ; home, the volume's Trashes and the e2e harness's roots.\n\
         (allow file-read-data{above})\n\
         (allow file-read-data file-write* (subpath \"{root}\") (subpath \"{USER_HOME}\")\n\
         \x20   (subpath \"{trash}\") {E2E_ROOTS})\n\
         ; What no job may change: the guard, these profiles, the runner's environment, and\n\
         ; the directories holding them, which a job could move away and remake.\n\
         (deny file-write* (subpath \"{guard}\") (literal \"{sandbox}\") (literal \"{try_sandbox}\")\n\
         \x20   (literal \"{env}\") (literal \"{root}\") (literal \"{runner}\"))\n\
         ; No socket of the person's, their agents' among them: none in a home, on a\n\
         ; volume or in /tmp, but the runner's own.\n\
         (deny network-outbound (subpath \"/Users\") (subpath \"/Volumes\") (subpath \"/private/tmp\"))\n\
         (allow network-outbound (subpath \"{root}\") (subpath \"{USER_HOME}\") {E2E_ROOTS})\n\
         ; None in /var/run but the resolver's and the system log's: not launchd's for each\n\
         ; login session, where the person's SSH agent is, nor Docker's or the iPhone's.\n\
         (deny network-outbound (subpath \"/private/var/run\"))\n",
        trash = layout.trash,
        guard = layout.guard,
        sandbox = layout.sandbox(),
        try_sandbox = layout.try_sandbox(),
        env = layout.runner.join(".env"),
        runner = layout.runner,
    );
    profile.push_str("(allow network-outbound");
    for socket in RUN_SOCKETS {
        let _written = write!(profile, " (literal \"{socket}\")");
    }
    profile.push_str(
        ")\n; No keychain and no other credential: none of the services that hold or hand one out.\n\
         (deny mach-lookup",
    );
    for service in CREDENTIAL_SERVICES {
        let _written = write!(profile, "\n    (global-name \"{service}\")");
    }
    for names in CREDENTIAL_SERVICE_NAMES {
        let _written = write!(profile, "\n    (global-name-regex #\"{names}\")");
    }
    profile.push_str(")\n");
    profile.push_str(
        "; The set-user-ID programs the tests start, which may not start in a sandbox.\n\
         (allow process-exec (with no-sandbox)",
    );
    for program in SETUID {
        let _written = write!(profile, " (literal \"{program}\")");
    }
    profile.push_str(")\n");
    if !ports.is_empty() {
        profile.push_str("; The person's services on loopback.\n(deny network-outbound");
        for port in ports {
            let _written = write!(profile, "\n    (remote tcp \"localhost:{port}\")");
        }
        profile.push_str(")\n");
    }
    profile
}

/// `try`'s sandbox: the daemon's `base`, and no socket in the person's own temporary and cache
/// directories, `user_dir` (theirs in `/var/folders`, which only the person may reach, so the
/// daemon's jobs never do), but in `try`'s `own`: none of their browser's, their terminal
/// multiplexer's or their tools' servers. `try` keeps its home and temporary directory there
/// ([`try_plist`]).
fn try_profile(base: &str, user_dir: &Utf8Path, own: &[&Utf8Path]) -> String {
    let own = own.iter().fold(String::new(), |mut out, dir| {
        let _written = write!(out, " (subpath \"{dir}\")");
        out
    });
    format!(
        "{base}; As the person: none of their sockets in their own temporary and cache\n\
         ; directories but this runner's.\n\
         (deny network-outbound (subpath \"{user_dir}\"))\n\
         (allow network-outbound{own})\n"
    )
}

/// The environment a runner's job starts from, whole: its home, its temporary directory, its
/// `PATH`, and who it runs as. Nothing of the login session's or launchd's own (the session's
/// SSH agent above all) reaches it ([`sandboxed_runner`]).
fn job_env(user: &str, home: &str, tmp: &str, layout: &Layout) -> [(&'static str, String); 6] {
    [
        ("HOME", home.to_owned()),
        ("TMPDIR", tmp.to_owned()),
        ("PATH", path(layout)),
        ("USER", user.to_owned()),
        ("LOGNAME", user.to_owned()),
        // The account has no login shell, and a test's terminal starts the one this names.
        ("SHELL", "/bin/zsh".to_owned()),
    ]
}

/// launchd's job for the runner: as [`USER`], in its home, in the sandbox, kept alive while the
/// volume holds the runner's registration, at `Nice 10` with low-priority I/O, with the
/// descriptors a build needs (launchd gives a job 256).
fn runner_plist(layout: &Layout) -> String {
    let env = job_env(USER, USER_HOME, &format!("{USER_TMP}/"), layout);
    let log = Utf8PathBuf::from(format!("{LOGS}/mac.log"));
    let args = sandboxed_runner(layout, &layout.sandbox(), &env);
    let alive_while = runner_alive(layout);
    let (dir, user) = (&layout.runner, Some(USER));
    plist(&Job {
        label: DAEMON,
        user,
        args: &args,
        env: &env,
        dir,
        run: Run::While(&alive_while),
        log: &log,
    })
}

/// `try`'s agent: the same runner and priority as the daemon, in its own sandbox
/// ([`try_profile`]), as the person `user`, with a home and a temporary directory of its own in
/// the person's temporary directory `temp`, on the startup disk as a hosted job's and the local
/// gate's are (the person's home is out of the sandbox's reach).
fn try_plist(layout: &Layout, temp: &Utf8Path, user: &str) -> String {
    let (home, tmp) = (temp.join(TRY_HOME), temp.join(TRY_TMP));
    let env = job_env(user, home.as_str(), &format!("{tmp}/"), layout);
    let args = sandboxed_runner(layout, &layout.try_sandbox(), &env);
    let log = layout.root.join("try.log");
    let alive_while = runner_alive(layout);
    let dir = &layout.runner;
    plist(&Job {
        label: TRY_AGENT,
        user: None,
        args: &args,
        env: &env,
        dir,
        run: Run::While(&alive_while),
        log: &log,
    })
}

/// The runner's `run.sh` in the sandbox `profile`, from `env` alone: launchd hands a job its
/// login session's environment, the SSH agent's socket among it, and a property list's
/// `EnvironmentVariables` adds to that but takes nothing away, so `env -i` starts it afresh.
fn sandboxed_runner(layout: &Layout, profile: &Utf8Path, env: &[(&str, String)]) -> Vec<String> {
    let start = ["/usr/bin/env".to_owned(), "-i".to_owned()];
    let env = env.iter().map(|(key, value)| format!("{key}={value}"));
    let sandbox = ["/usr/bin/sandbox-exec".to_owned(), "-f".to_owned(), profile.to_string()];
    start
        .into_iter()
        .chain(env)
        .chain(sandbox)
        .chain([layout.runner.join("run.sh").to_string()])
        .collect()
}

/// What keeps the runner alive: its registration, on the volume. Until it is registered, or
/// while the volume is not attached, launchd does not start it.
fn runner_alive(layout: &Layout) -> Utf8PathBuf {
    layout.runner.join(".runner")
}

/// launchd's job for the Linux runner's VM: the guard's `xtask warm linux run` as [`USER`],
/// which runs tart headless and lowers the VM's own process, kept alive while the volume holds
/// the VM, at `Nice 10` with low-priority I/O.
fn vm_plist(home: &Utf8Path, layout: &Layout) -> String {
    let tart_home = super::linux::tart_home(home);
    let config = tart_home.join("vms").join(super::LINUX_LABEL).join("config.json");
    let args = [
        layout.guard.join("xtask").to_string(),
        "warm".to_owned(),
        "linux".to_owned(),
        "run".to_owned(),
    ];
    let env = [
        ("HOME", USER_HOME.to_owned()),
        ("PATH", "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_owned()),
    ];
    let log = Utf8PathBuf::from(format!("{LOGS}/linux.log"));
    let dir = home.join("linux");
    let user = Some(USER);
    plist(&Job {
        label: VM_DAEMON,
        user,
        args: &args,
        env: &env,
        dir: &dir,
        run: Run::While(&config),
        log: &log,
    })
}

/// launchd's job for the volume: as root, when loaded (at every start of the Mac) and at every
/// mount (the external disk's among them), `image` attached at [`super::VOLUME`] unless it is or
/// the disk is not there. A launchd job may not open a file on an external disk, but `diskutil`
/// has the disk-image service open the image, which may.
fn volume_plist(image: &Utf8Path) -> Result<String> {
    anyhow::ensure!(!image.as_str().contains('\''), "the image's path {image} holds a quote");
    let attach = super::attach_args(image).iter().fold(String::new(), |mut out, arg| {
        let _written = write!(out, " '{arg}'");
        out
    });
    let volume = super::VOLUME;
    let script =
        format!("[ -d '{volume}/mac' ] || [ ! -e '{image}' ] || exec /usr/sbin/diskutil{attach}");
    let args = ["/bin/sh".to_owned(), "-c".to_owned(), script];
    let log = Utf8PathBuf::from(format!("{LOGS}/volume.log"));
    Ok(plist(&Job {
        label: VOLUME_DAEMON,
        user: None,
        args: &args,
        env: &[],
        dir: Utf8Path::new("/"),
        run: Run::AtMounts,
        log: &log,
    }))
}

/// launchd's job for [`USER`]'s pasteboard server: macOS's own, run as [`USER`] when a process
/// first looks up [`PASTEBOARD_SERVICE`] in the system domain. A login session's processes find
/// their session's server first, so the person's pasteboard stays theirs and this one the
/// runner's: its tests' pasteboards, named and general, are [`USER`]'s.
fn pboard_plist() -> String {
    let args = [PBOARD.to_owned()];
    let env = [("HOME", USER_HOME.to_owned())];
    let log = Utf8PathBuf::from(format!("{LOGS}/pboard.log"));
    plist(&Job {
        label: PBOARD_DAEMON,
        user: Some(USER),
        args: &args,
        env: &env,
        dir: Utf8Path::new(USER_HOME),
        run: Run::OnLookup(&[PASTEBOARD_SERVICE]),
        log: &log,
    })
}

/// The daemons, each label with its property list: the volume's first, which the others run
/// from, and the pasteboard's before the runner's, whose tests use it.
fn daemons(
    home: &Utf8Path,
    layout: &Layout,
    image: &Utf8Path,
) -> Result<[(&'static str, String); 4]> {
    Ok([
        (VOLUME_DAEMON, volume_plist(image)?),
        (PBOARD_DAEMON, pboard_plist()),
        (DAEMON, runner_plist(layout)),
        (VM_DAEMON, vm_plist(home, layout)),
    ])
}

/// A launchd job: a daemon run as `user` (as root with none), or an agent of whoever loads it.
struct Job<'a> {
    label: &'a str,
    user: Option<&'a str>,
    args: &'a [String],
    env: &'a [(&'a str, String)],
    /// Its working directory.
    dir: &'a Utf8Path,
    run: Run<'a>,
    /// Its output.
    log: &'a Utf8Path,
}

/// When launchd runs a job.
#[derive(Clone, Copy)]
enum Run<'a> {
    /// While this path exists, and again whenever it exits.
    While(&'a Utf8Path),
    /// When it is loaded, and whenever a volume is mounted.
    AtMounts,
    /// When a process first looks up one of these Mach services, which it then serves.
    OnLookup(&'a [&'a str]),
}

/// `job`'s property list.
fn plist(job: &Job<'_>) -> String {
    let Job { label, user, args, env, dir, run, log } = *job;
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
    if let Some(user) = user {
        key("UserName", &format!("<string>{user}</string>"));
        key("GroupName", &format!("<string>{user}</string>"));
    }
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
    match run {
        Run::While(path) => key(
            "KeepAlive",
            &format!(
                "<dict><key>PathState</key><dict><key>{}</key><true/></dict></dict>",
                escape(path.as_str())
            ),
        ),
        Run::AtMounts => {
            key("RunAtLoad", "<true/>");
            key("StartOnMount", "<true/>");
        }
        Run::OnLookup(services) => {
            let services = services.iter().fold(String::new(), |mut out, service| {
                let _written = write!(out, "<key>{service}</key><true/>");
                out
            });
            key("MachServices", &format!("<dict>{services}</dict>"));
        }
    }
    key("ThrottleInterval", "<integer>30</integer>");
    key("Nice", "<integer>10</integer>");
    key("LowPriorityIO", "<true/>");
    key("ProcessType", "<string>Standard</string>");
    let files = "<dict><key>NumberOfFiles</key><integer>65536</integer></dict>";
    key("SoftResourceLimits", files);
    key("HardResourceLimits", files);
    let log = escape(log.as_str());
    key("StandardOutPath", &format!("<string>{log}</string>"));
    key("StandardErrorPath", &format!("<string>{log}</string>"));
    text.push_str("</dict>\n</plist>\n");
    text
}

/// `text` as XML character data.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Whether the administrator's part is done as `install` would write it: the user, the log
/// directory, and each daemon installed as `daemons` hold it.
fn admin_done(daemons: &[(&str, String)]) -> bool {
    let user = std::process::Command::new("dscl")
        .args([".", "-read", &format!("/Users/{USER}"), "UniqueID"])
        .output()
        .is_ok_and(|out| out.status.success());
    user && Utf8Path::new(LOGS).is_dir()
        && daemons.iter().all(|(label, ours)| {
            std::fs::read_to_string(installed(label)).ok().as_deref() == Some(ours.as_str())
        })
}

/// Where launchd's system domain takes `label`'s property list from.
fn installed(label: &str) -> String {
    format!("/Library/LaunchDaemons/{label}.plist")
}

/// Whether anything of the administrator's part is on this Mac: [`USER`], the log directory, or
/// a daemon's property list.
pub fn administered() -> bool {
    let user = std::process::Command::new("dscl")
        .args([".", "-read", &format!("/Users/{USER}"), "UniqueID"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    let labels = [VOLUME_DAEMON, PBOARD_DAEMON, DAEMON, VM_DAEMON];
    user || Utf8Path::new(LOGS).exists()
        || labels.iter().any(|l| Utf8Path::new(&installed(l)).exists())
}

/// The administrator's part of `xtask warm uninstall`, as a script `sh` runs as root from `path`:
/// each daemon booted out and its property list removed, the VM's first and the volume's last,
/// then the volume detached, the log directory removed, and [`USER`], its group and its home.
/// Each step is skipped once done, and it reads nothing but itself.
pub fn uninstall_script(path: &Utf8Path) -> String {
    let volume = super::VOLUME;
    let mut script = format!(
        "#!/bin/sh\n\
         # The warm runners' administrator part, undone (`cargo xtask warm uninstall` writes it):\n\
         # the daemons, the volume, the log directory and the account {USER}. Run it as root;\n\
         # each step is skipped once done.\n\
         set -eu\n\
         [ \"$(id -u)\" = 0 ] || {{ echo 'run it as root: sudo sh {path}' >&2; exit 1; }}\n\n\
         # The daemons: the VM's first, which stops the VM, and the volume's last.\n"
    );
    for label in [VM_DAEMON, DAEMON, PBOARD_DAEMON, VOLUME_DAEMON] {
        let _written = writeln!(
            script,
            "launchctl bootout system/{label} 2>/dev/null || true\n/bin/rm -f {}",
            installed(label)
        );
    }
    let _written = write!(
        script,
        "\n# The volume, which nothing runs from now.\n\
         if mount | grep -q ' on {volume} '; then\n\
         \x20 hdiutil detach {volume} || hdiutil detach -force {volume}\n\
         fi\n\n\
         # The log directory, and the account with its group and home.\n\
         /bin/rm -rf {LOGS}\n\
         if dscl . -read /Users/{USER} UniqueID >/dev/null 2>&1; then\n\
         \x20 dscl . -delete /Users/{USER}\n\
         fi\n\
         if dscl . -read /Groups/{USER} PrimaryGroupID >/dev/null 2>&1; then\n\
         \x20 dscl . -delete /Groups/{USER}\n\
         fi\n\
         /bin/rm -rf {USER_HOME}\n"
    );
    script
}

/// The administrator's part, as a script `sh` runs as root from `path`: [`USER`] and its group
/// unless they exist ([`create_user`]), its home on the startup disk, the log directory, and
/// each of `daemons` installed and loaded unless it is already as written, the volume's first,
/// whose volume the others wait for. Each step is skipped once done, so it runs again after
/// every `install`. It reads nothing but itself: the property lists are in it, never read from
/// the volume the runner's jobs write.
pub fn admin_script(daemons: &[(&str, String)], path: &Utf8Path) -> String {
    let mut script = format!(
        "#!/bin/sh\n\
         # The warm runners' administrator part (`cargo xtask warm mac install` writes it): the\n\
         # account {USER}, its home, the log directory and the daemons, whose property lists\n\
         # are below. Run it as root, once and after every install; each step is skipped once\n\
         # done. It reads nothing but itself.\n\
         set -eu\n\
         [ \"$(id -u)\" = 0 ] || {{ echo 'run it as root: sudo sh {path}' >&2; exit 1; }}\n\n\
         # The account: a group of its own, outside staff; no password or shell; hidden. An ID\n\
         # another account holds stops it here.\n\
         taken=$(dscl . -search /Users UniqueID {ID} | awk 'NR==1 {{print $1}}')\n\
         if [ -n \"$taken\" ] && [ \"$taken\" != {USER} ]; then\n\
         \x20 echo \"user ID {ID} is $taken's\" >&2; exit 1\n\
         fi\n\
         if ! dscl . -read /Groups/{USER} PrimaryGroupID >/dev/null 2>&1; then\n",
    );
    let (group, user): (Vec<String>, Vec<String>) =
        create_user().into_iter().partition(|line| line.contains("/Groups/"));
    for line in group {
        let _written = writeln!(script, "  {line}");
    }
    let _written =
        writeln!(script, "fi\nif ! dscl . -read /Users/{USER} UniqueID >/dev/null 2>&1; then");
    for line in user {
        let _written = writeln!(script, "  {line}");
    }
    let volume = super::VOLUME;
    let _written = write!(
        script,
        "fi\n\n\
         # Its home and temporary directory, which only it may read.\n\
         mkdir -p {USER_TMP}\n\
         chown {USER}:{USER} {USER_HOME} {USER_TMP}\n\
         chmod 700 {USER_HOME} {USER_TMP}\n\n\
         # The daemons' log directory.\n\
         mkdir -p {LOGS}\n\
         chown {USER}:{USER} {LOGS}\n\n\
         # Each daemon, from the property list staged before it: replaced when that changed, and\n\
         # loaded.\n\
         staged=$(mktemp)\n\
         trap '/bin/rm -f \"$staged\"' EXIT\n\
         daemon() {{\n\
         \x20 if ! cmp -s \"$staged\" \"/Library/LaunchDaemons/$1.plist\"; then\n\
         \x20   launchctl bootout \"system/$1\" 2>/dev/null || true\n\
         \x20   install -m 644 -o root -g wheel \"$staged\" \"/Library/LaunchDaemons/$1.plist\"\n\
         \x20 fi\n\
         \x20 launchctl print \"system/$1\" >/dev/null 2>&1 ||\n\
         \x20   launchctl bootstrap system \"/Library/LaunchDaemons/$1.plist\"\n\
         }}\n"
    );
    for (index, (label, plist)) in daemons.iter().enumerate() {
        let _written =
            write!(script, "\ncat >\"$staged\" <<'PLIST'\n{plist}PLIST\ndaemon {label}\n");
        if index == 0 {
            let _written = write!(
                script,
                "\n# The volume, which its daemon attaches when loaded, before the daemons that\n\
                 # run from it.\n\
                 waited=0\n\
                 until [ -d {volume}/mac ]; do\n\
                 \x20 if [ \"$waited\" -ge 60 ]; then\n\
                 \x20   echo 'the volume is not attached at {volume}: see {LOGS}/volume.log' >&2\n\
                 \x20   exit 1\n\
                 \x20 fi\n\
                 \x20 sleep 1\n\
                 \x20 waited=$((waited + 1))\n\
                 done\n"
            );
        }
    }
    script
}

/// The `dscl` lines that make [`USER`]: a group of its own, so it is outside `staff`; no
/// password and no shell, so nobody logs in as it; hidden; its home on the startup disk.
fn create_user() -> Vec<String> {
    let id = ID.to_string();
    [
        format!("dscl . -create /Groups/{USER} PrimaryGroupID {id}"),
        format!("dscl . -create /Groups/{USER} RealName 'Slopty CI'"),
        format!("dscl . -create /Groups/{USER} Password '*'"),
        format!("dscl . -create /Users/{USER} UniqueID {id}"),
        format!("dscl . -create /Users/{USER} PrimaryGroupID {id}"),
        format!("dscl . -create /Users/{USER} RealName 'Slopty CI runner'"),
        format!("dscl . -create /Users/{USER} NFSHomeDirectory {USER_HOME}"),
        format!("dscl . -create /Users/{USER} UserShell /usr/bin/false"),
        format!("dscl . -create /Users/{USER} Password '*'"),
        format!("dscl . -create /Users/{USER} IsHidden 1"),
    ]
    .into()
}

/// `xtask warm mac register`: the runner registered as [`MAC_LABEL`] with that label alone, not
/// the `self-hosted` and the system's every self-hosted runner takes, so a workflow reaches it
/// only by its name; the token passed in the environment `config.sh` reads, never on its command
/// line; then its own `.env` again, over the one `config.sh` writes, with the disk `image` is on.
pub fn register(sh: &Shell, home: &Utf8Path, image: &Utf8Path) -> Result<()> {
    let layout = Layout::of(home);
    anyhow::ensure!(
        layout.runner.join("config.sh").is_file(),
        "no runner in {}: `cargo xtask warm mac install` first",
        layout.runner
    );
    let config = super::repository(sh)?;
    let token = super::registration_token(sh, &config.repository)?;
    // `config.sh` refuses a runner that holds a registration, and `try`'s would hold the old one.
    let trying = try_loaded(sh);
    if trying {
        try_run(sh, home, true)?;
    }
    forget_registration(home)?;
    let url = format!("https://github.com/{}", config.repository);
    let runner = Shell::new()?;
    runner.change_dir(&layout.runner);
    cmd!(
        runner,
        "./config.sh --unattended --replace --url {url} --name {MAC_LABEL} --labels {MAC_LABEL} --no-default-labels --work _work"
    )
    .env("ACTIONS_RUNNER_INPUT_TOKEN", token)
    .quiet()
    .run()
    .context("register the Mac runner")?;
    runner_environment(&layout, &super::mount_point(image)?)?;
    println!("  ✓ {MAC_LABEL} registered with {}", config.repository);
    if trying {
        try_run(sh, home, false)?;
    }
    Ok(())
}

/// `try`'s agent stopped, and its property list, home and temporary directory removed from the
/// person's temporary directory.
pub fn remove_try(sh: &Shell) -> Result<()> {
    if try_loaded(sh) {
        let uid = rustix::process::getuid().as_raw().to_string();
        cmd!(sh, "launchctl bootout gui/{uid}/{TRY_AGENT}").quiet().run()?;
    }
    let temp = cmd!(sh, "getconf DARWIN_USER_TEMP_DIR").quiet().read()?;
    let temp = Utf8PathBuf::from(temp.trim().trim_end_matches('/'));
    let plist = temp.join(format!("{TRY_AGENT}.plist"));
    if plist.exists() {
        std::fs::remove_file(&plist).with_context(|| format!("remove {plist}"))?;
    }
    for dir in [TRY_HOME, TRY_TMP] {
        let dir = temp.join(dir);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("remove {dir}"))?;
        }
    }
    Ok(())
}

/// The Mac runner's registration taken off the volume at `home`, so that no daemon or agent
/// starts it again ([`runner_alive`]).
pub fn forget_registration(home: &Utf8Path) -> Result<()> {
    let layout = Layout::of(home);
    for file in super::REGISTRATION {
        let file = layout.runner.join(file);
        match std::fs::remove_file(&file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error).with_context(|| format!("remove {file}"));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether `try`'s agent is loaded in the person's login session.
fn try_loaded(sh: &Shell) -> bool {
    let uid = rustix::process::getuid().as_raw().to_string();
    cmd!(sh, "launchctl print gui/{uid}/{TRY_AGENT}")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .is_ok()
}

/// `xtask warm mac try`: the runner as the person, in its sandbox at its priority, through an
/// agent of their login session, until `--stop` or the daemon takes over (the administrator's
/// part boots this agent out first).
pub fn try_run(sh: &Shell, home: &Utf8Path, stop: bool) -> Result<()> {
    let layout = Layout::of(home);
    let uid = rustix::process::getuid().as_raw().to_string();
    let domain = format!("gui/{uid}");
    if try_loaded(sh) {
        cmd!(sh, "launchctl bootout {domain}/{TRY_AGENT}").quiet().run()?;
        println!("  ✓ {TRY_AGENT} stopped");
    }
    if stop {
        return Ok(());
    }
    anyhow::ensure!(
        runner_alive(&layout).is_file(),
        "{MAC_LABEL} is not registered: `cargo xtask warm mac register` first"
    );
    let sandbox = layout.sandbox();
    anyhow::ensure!(sandbox.is_file(), "no {sandbox}: `cargo xtask warm mac install` first");
    let base = std::fs::read_to_string(&sandbox).with_context(|| format!("read {sandbox}"))?;
    // launchd refuses a property list on a volume that ignores ownership, and one there could be
    // a job's: written by this binary into the person's own temporary directory, which launchd
    // reads once.
    let temp = cmd!(sh, "getconf DARWIN_USER_TEMP_DIR").quiet().read()?;
    let temp = Utf8PathBuf::from(temp.trim().trim_end_matches('/'));
    let mut own = Vec::new();
    for dir in [TRY_HOME, TRY_TMP] {
        let dir = temp.join(dir);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {dir}"))?;
        own.push(canonical(&dir)?);
    }
    // The sandbox matches a path as it resolves, `/private/var` for `/var`.
    let resolved = canonical(&temp)?;
    let user_dir = resolved.parent().context("the temporary directory has no parent")?;
    let own: Vec<&Utf8Path> = own.iter().map(Utf8PathBuf::as_path).collect();
    write(&layout.try_sandbox(), &try_profile(&base, user_dir, &own))?;
    let user = cmd!(sh, "id -un").quiet().read()?;
    let staged = temp.join(format!("{TRY_AGENT}.plist"));
    write(&staged, &try_plist(&layout, &temp, user.trim()))?;
    cmd!(sh, "launchctl bootstrap {domain} {staged}").quiet().run()?;
    println!(
        "  ✓ {MAC_LABEL} runs as you in its sandbox; its log: {}",
        layout.root.join("try.log")
    );
    Ok(())
}

/// `path` with every link resolved.
fn canonical(path: &Utf8Path) -> Result<Utf8PathBuf> {
    let resolved = std::fs::canonicalize(path).with_context(|| format!("resolve {path}"))?;
    Utf8PathBuf::from_path_buf(resolved)
        .map_err(|p| anyhow::anyhow!("{} is not UTF-8", p.display()))
}

/// The Mac daemon's state as launchd reports it, `try`'s agent's, and the tail of its log.
pub fn status(sh: &Shell, home: &Utf8Path) {
    let layout = Layout::of(home);
    let state = |target: &str| {
        cmd!(sh, "launchctl print {target}").quiet().ignore_stderr().read().ok().and_then(|out| {
            out.lines().find_map(|l| l.trim().strip_prefix("state = ").map(str::to_owned))
        })
    };
    let uid = rustix::process::getuid().as_raw();
    println!("▶ the Mac runner ({})", layout.root);
    let daemons = [("volume", VOLUME_DAEMON), ("pasteboard", PBOARD_DAEMON), ("daemon", DAEMON)];
    for (what, label) in daemons {
        let loaded = state(&format!("system/{label}"));
        println!("  {what}: {}", loaded.as_deref().unwrap_or("not loaded"));
    }
    if let Some(tried) = state(&format!("gui/{uid}/{TRY_AGENT}")) {
        println!("  as you ({TRY_AGENT}): {tried}");
    }
    let registered = runner_alive(&layout).is_file();
    println!("  registered here: {}", if registered { "yes" } else { "no" });
    for log in [Utf8PathBuf::from(format!("{LOGS}/mac.log")), layout.root.join("try.log")] {
        if let Ok(text) = std::fs::read_to_string(&log) {
            let lines: Vec<&str> = text.lines().collect();
            for line in lines.iter().skip(lines.len().saturating_sub(3)) {
                println!("  {}: {line}", log.file_name().unwrap_or_default());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use super::{
        Job, Layout, Run, admin_script, create_user, daemons, environment, pboard_plist, plist,
        runner_plist, sandbox_profile, try_plist, try_profile, uninstall_script, vm_plist,
        volume_plist,
    };

    const HOME: &str = "/Volumes/slopty-ci";
    const IMAGE: &str = "/Volumes/Build/slopty-ci.sparsebundle";

    fn layout() -> Layout {
        Layout::of(Utf8Path::new(HOME))
    }

    /// The sandbox denies the person's volumes, homes, `/tmp` files, sockets and services, gives
    /// the runner its own directory, home and Trashes back, keeps the guard, the runner's
    /// environment and the directories holding them out of its jobs' reach, in that order, since
    /// the last rule that matches decides, and lets `ps` and `login` start outside it.
    #[test]
    fn the_sandbox_denies_the_person_s_files_and_services_and_the_guard() {
        let profile = sandbox_profile(&layout(), &[4226, 9333]);
        let at = |needle: &str| profile.find(needle).unwrap_or_else(|| panic!("{needle}"));
        let denied =
            at("(deny file-read-data file-write* (subpath \"/Volumes\") (subpath \"/Users\")");
        let tmp = at("(regex #\"^/private/tmp/.\")");
        let above = at("(allow file-read-data (literal \"/Volumes/slopty-ci\") \
             (literal \"/Volumes\"))");
        let own = at("(allow file-read-data file-write* (subpath \"/Volumes/slopty-ci/mac\") \
             (subpath \"/Users/slopty-ci\")\n    (subpath \"/Volumes/slopty-ci/.Trashes\") \
             (regex #\"^/private/tmp/slopty-e2e-\"))");
        let guard = at("(deny file-write* (subpath \"/Volumes/slopty-ci/mac/guard\")");
        let sockets = at("(deny network-outbound (subpath \"/Users\") (subpath \"/Volumes\") \
             (subpath \"/private/tmp\"))");
        let own_sockets = at("(allow network-outbound (subpath \"/Volumes/slopty-ci/mac\") \
             (subpath \"/Users/slopty-ci\") (regex #\"^/private/tmp/slopty-e2e-\"))");
        let ports = at("(remote tcp \"localhost:4226\")");
        assert!(
            denied < tmp
                && tmp < above
                && above < own
                && own < guard
                && guard < sockets
                && sockets < own_sockets
                && own_sockets < ports,
            "{profile}"
        );
        assert!(!profile.contains("(allow file-read*"), "file-read* never undoes file-read-data");
        for kept in [
            "(literal \"/Volumes/slopty-ci/mac/sandbox.sb\")",
            "(literal \"/Volumes/slopty-ci/mac/runner/.env\")",
            "(literal \"/Volumes/slopty-ci/mac\") (literal \"/Volumes/slopty-ci/mac/runner\"))",
        ] {
            let protected = profile.get(guard..sockets).unwrap_or_default();
            assert!(protected.contains(kept), "{kept} in {profile}");
        }
        assert!(profile.contains(
            "(allow process-exec (with no-sandbox) (literal \"/bin/ps\") (literal \"/usr/bin/login\"))"
        ));
        assert!(profile.contains("(remote tcp \"localhost:9333\")"), "{profile}");
        assert!(!sandbox_profile(&layout(), &[]).contains("(remote tcp"));
        let run = at("(deny network-outbound (subpath \"/private/var/run\"))");
        let resolver = at("(allow network-outbound (literal \"/private/var/run/mDNSResponder\") \
             (literal \"/private/var/run/syslog\"))");
        let keychain = at("(deny mach-lookup\n    (global-name \"com.apple.systemkeychain\")");
        assert!(own_sockets < run && run < resolver && resolver < keychain, "{profile}");
        for needle in [
            "(global-name-regex #\"^com\\.apple\\.securityd(\\.|$)\")",
            "(global-name \"com.apple.authd\")",
            "(literal \"/Volumes/slopty-ci/mac/try.sb\")",
        ] {
            assert!(profile.contains(needle), "{needle} in {profile}");
        }
        for kept in ["trustd", "syspolicy", "opendirectoryd", "pasteboard", "SecurityServer"] {
            assert!(!profile.contains(kept), "{kept} stays reachable: {profile}");
        }
    }

    /// `try`'s sandbox is the daemon's, then no socket in the person's own temporary and cache
    /// directories but `try`'s own, after it so that those win.
    #[test]
    fn try_s_sandbox_keeps_the_person_s_sockets_out() {
        let base = sandbox_profile(&layout(), &[]);
        let user_dir = Utf8Path::new("/private/var/folders/0_/x");
        let own = [Utf8Path::new("/private/var/folders/0_/x/T/slopty-warm-home")];
        let profile = try_profile(&base, user_dir, &own);
        assert!(profile.starts_with(&base), "{profile}");
        let tail = profile.get(base.len()..).unwrap_or_default();
        let denied = tail.find("(deny network-outbound (subpath \"/private/var/folders/0_/x\"))");
        let allowed = tail.find(
            "(allow network-outbound (subpath \"/private/var/folders/0_/x/T/slopty-warm-home\"))",
        );
        assert!(denied.is_some_and(|d| allowed.is_some_and(|a| d < a)), "{tail}");
    }

    /// The runner's environment names its own toolchains and caches on the volume, turns
    /// incremental builds on, caps the jobs and the target dir, sets the hook, leaves `HOME` and
    /// `TMPDIR` to each job's property list, and holds no empty value, which the runner drops.
    #[test]
    fn the_runner_s_environment_is_its_own() {
        let env = environment(&layout(), Utf8Path::new("/Volumes/Build"));
        let get = |key: &str| env.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str());
        assert_eq!(get("HOME"), None);
        assert_eq!(get("TMPDIR"), None);
        assert_eq!(get("CARGO_HOME"), Some("/Volumes/slopty-ci/mac/cargo"));
        assert_eq!(get("RUSTUP_HOME"), Some("/Volumes/slopty-ci/mac/rustup"));
        assert_eq!(get("XDG_CACHE_HOME"), Some("/Volumes/slopty-ci/mac/cache"));
        assert_eq!(get("CARGO_INCREMENTAL"), Some("1"));
        assert_eq!(get("CARGO_BUILD_JOBS"), Some("8"));
        assert_eq!(get("SLOPTY_TARGET_BUDGET_GB"), Some("70"));
        assert_eq!(get("SLOPTY_DISK_FLOOR_GB"), Some("15"));
        assert_eq!(get("SLOPTY_WARM_DISK"), Some("/Volumes/Build"));
        assert!(env.iter().all(|(_, v)| !v.is_empty()), "{env:?}");
        assert_eq!(
            get("ACTIONS_RUNNER_HOOK_JOB_STARTED"),
            Some("/Volumes/slopty-ci/mac/guard/started.sh")
        );
    }

    /// The daemons run the runner in the sandbox, from an environment of its own alone, and the
    /// VM through the guard's xtask, as the runner's user in its home, at `Nice 10` with
    /// low-priority I/O and `Standard`, never `Background`; `try` runs the same as the person, in
    /// its own sandbox, with a home and a temporary directory of its own in theirs and a log of its
    /// own; and XML is escaped.
    #[test]
    fn the_daemon_runs_sandboxed_niced_and_on_every_core() {
        let text = runner_plist(&layout());
        for needle in [
            "<key>UserName</key><string>slopty-ci</string>",
            "<key>ProgramArguments</key><array><string>/usr/bin/env</string><string>-i</string>\
             <string>HOME=/Users/slopty-ci</string><string>TMPDIR=/Users/slopty-ci/tmp/</string>",
            "<string>SHELL=/bin/zsh</string><string>/usr/bin/sandbox-exec</string>\
             <string>-f</string><string>/Volumes/slopty-ci/mac/sandbox.sb</string>\
             <string>/Volumes/slopty-ci/mac/runner/run.sh</string></array>",
            "<string>USER=slopty-ci</string><string>LOGNAME=slopty-ci</string>",
            "<key>HOME</key><string>/Users/slopty-ci</string>",
            "<key>TMPDIR</key><string>/Users/slopty-ci/tmp/</string>",
            "<key>PATH</key><string>/Volumes/slopty-ci/mac/cargo/bin:/opt/homebrew/bin:",
            "<key>Nice</key><integer>10</integer>",
            "<key>LowPriorityIO</key><true/>",
            "<key>ProcessType</key><string>Standard</string>",
            "<key>PathState</key><dict><key>/Volumes/slopty-ci/mac/runner/.runner</key>",
            "<key>NumberOfFiles</key><integer>65536</integer>",
            "<string>/Library/Logs/slopty-warm/mac.log</string>",
        ] {
            assert!(text.contains(needle), "{needle} in {text}");
        }
        let tried = try_plist(&layout(), Utf8Path::new("/var/folders/x/T"), "person");
        assert!(!tried.contains("UserName"), "{tried}");
        for needle in [
            "<key>Label</key><string>com.slopty.warm-mac.try</string>",
            "<string>/usr/bin/env</string><string>-i</string>\
             <string>HOME=/var/folders/x/T/slopty-warm-home</string>\
             <string>TMPDIR=/var/folders/x/T/warm/</string>",
            "<string>USER=person</string><string>LOGNAME=person</string>",
            "<key>HOME</key><string>/var/folders/x/T/slopty-warm-home</string>",
            "<key>TMPDIR</key><string>/var/folders/x/T/warm/</string>",
            "<key>PATH</key><string>/Volumes/slopty-ci/mac/cargo/bin:",
            "<string>/Volumes/slopty-ci/mac/try.log</string>",
            "<key>Nice</key><integer>10</integer>",
            "<string>-f</string><string>/Volumes/slopty-ci/mac/try.sb</string>",
        ] {
            assert!(tried.contains(needle), "{needle} in {tried}");
        }
        for text in [&text, &tried] {
            assert!(!text.contains("SSH_AUTH_SOCK"), "{text}");
        }
        let vm = vm_plist(Utf8Path::new(HOME), &layout());
        for needle in [
            "<key>UserName</key><string>slopty-ci</string>",
            "<string>/Volumes/slopty-ci/mac/guard/xtask</string><string>warm</string>\
             <string>linux</string><string>run</string>",
            "<key>Nice</key><integer>10</integer>",
            "<key>PathState</key><dict>\
             <key>/Volumes/slopty-ci/linux/tart/vms/slopty-warm-linux/config.json</key>",
            "<string>/Library/Logs/slopty-warm/linux.log</string>",
        ] {
            assert!(vm.contains(needle), "{needle} in {vm}");
        }
        let (dir, log) = (Utf8Path::new("/d"), Utf8Path::new("/l"));
        let args = ["a&b".to_owned()];
        let run = Run::While(Utf8Path::new("/k"));
        let odd = plist(&Job { label: "x", user: None, args: &args, env: &[], dir, run, log });
        assert!(odd.contains("<string>a&amp;b</string>"), "{odd}");
    }

    /// The volume's daemon runs as root when loaded and at every mount, and attaches the image at
    /// the volume, ownership off and no set-user-ID program or device on it, unless it is
    /// attached or its disk is not; an image path a quote would break out of is refused.
    #[test]
    fn the_volume_is_attached_at_every_start_and_mount() {
        let text = volume_plist(Utf8Path::new(IMAGE)).expect("a plist");
        assert!(!text.contains("UserName"), "root: {text}");
        assert!(!text.contains("KeepAlive"), "{text}");
        for needle in [
            "<key>Label</key><string>com.slopty.warm-volume</string>",
            "<key>RunAtLoad</key><true/>",
            "<key>StartOnMount</key><true/>",
            "<string>/bin/sh</string><string>-c</string><string>\
             [ -d '/Volumes/slopty-ci/mac' ] || [ ! -e '/Volumes/Build/slopty-ci.sparsebundle' ] \
             || exec /usr/sbin/diskutil 'image' 'attach' '--nobrowse' '--mountPoint' \
             '/Volumes/slopty-ci' '--mountOptions' 'noowners,nosuid,nodev' \
             '/Volumes/Build/slopty-ci.sparsebundle'</string>",
            "<string>/Library/Logs/slopty-warm/volume.log</string>",
        ] {
            assert!(text.contains(needle), "{needle} in {text}");
        }
        let refused = volume_plist(Utf8Path::new("/Volumes/it's.sparsebundle"))
            .expect_err("a path with a quote is refused");
        assert!(refused.to_string().contains("holds a quote"), "{refused}");
    }

    /// The runner's pasteboard server is macOS's own, as the runner's user, started by the first
    /// lookup of the service AppKit asks for, never at load or kept alive.
    #[test]
    fn the_runner_has_a_pasteboard_server_of_its_own() {
        let text = pboard_plist();
        for needle in [
            "<key>Label</key><string>com.slopty.warm-pboard</string>",
            "<key>UserName</key><string>slopty-ci</string>",
            "<key>ProgramArguments</key><array><string>/usr/libexec/pboard</string></array>",
            "<key>MachServices</key><dict><key>com.apple.pasteboard.1</key><true/></dict>",
            "<string>/Library/Logs/slopty-warm/pboard.log</string>",
        ] {
            assert!(text.contains(needle), "{needle} in {text}");
        }
        for absent in ["KeepAlive", "RunAtLoad", "sandbox-exec"] {
            assert!(!text.contains(absent), "{absent} in {text}");
        }
    }

    /// The administrator's part of uninstalling boots out every daemon and removes its list, the
    /// VM's first and the volume's last, detaches the volume after them, and removes the account
    /// and its home only when they are there; it refuses to run but as root.
    #[test]
    fn the_uninstall_undoes_the_administrator_s_part() {
        let script = uninstall_script(Utf8Path::new("/c/target/warm/uninstall.sh"));
        let at = |needle: &str| script.find(needle).unwrap_or_else(|| panic!("{needle}"));
        let vm = at("launchctl bootout system/com.slopty.warm-linux 2>/dev/null || true\n");
        let mac = at("launchctl bootout system/com.slopty.warm-mac 2>/dev/null || true\n");
        let volume = at("launchctl bootout system/com.slopty.warm-volume 2>/dev/null || true\n");
        let detach = at("hdiutil detach /Volumes/slopty-ci || hdiutil detach -force");
        let user = at("dscl . -delete /Users/slopty-ci\n");
        let home = at("/bin/rm -rf /Users/slopty-ci\n");
        assert!(vm < mac && mac < volume && volume < detach && detach < user && user < home);
        for needle in [
            "set -eu\n",
            "sudo sh /c/target/warm/uninstall.sh",
            "/bin/rm -f /Library/LaunchDaemons/com.slopty.warm-pboard.plist\n",
            "if dscl . -read /Users/slopty-ci UniqueID >/dev/null 2>&1; then",
            "dscl . -delete /Groups/slopty-ci\n",
            "/bin/rm -rf /Library/Logs/slopty-warm\n",
        ] {
            assert!(script.contains(needle), "{needle} in {script}");
        }
    }

    /// The user is made outside `staff`, with no password or shell, hidden, its home on the
    /// startup disk; the administrator's script makes it and its home only when missing, refuses
    /// an ID another account holds, holds every daemon's property list itself, replaces a daemon
    /// only when its list changed, and loads the volume's first and waits for the volume.
    #[test]
    fn the_user_has_a_group_of_its_own_and_no_login() {
        let lines = create_user();
        for needle in [
            "dscl . -create /Groups/slopty-ci PrimaryGroupID 600",
            "dscl . -create /Users/slopty-ci PrimaryGroupID 600",
            "dscl . -create /Users/slopty-ci UserShell /usr/bin/false",
            "dscl . -create /Users/slopty-ci Password '*'",
            "dscl . -create /Users/slopty-ci IsHidden 1",
            "dscl . -create /Users/slopty-ci NFSHomeDirectory /Users/slopty-ci",
        ] {
            assert!(lines.iter().any(|l| l == needle), "{needle}");
        }
        assert!(!lines.iter().any(|l| l.contains(" 20")), "never staff's group 20");
        let layout = layout();
        let daemons =
            daemons(Utf8Path::new(HOME), &layout, Utf8Path::new(IMAGE)).expect("the daemons");
        let script = admin_script(&daemons, Utf8Path::new("/c/target/warm/admin.sh"));
        let at = |needle: &str| script.find(needle).unwrap_or_else(|| panic!("{needle}"));
        let group = at("if ! dscl . -read /Groups/slopty-ci PrimaryGroupID");
        let made = at("  dscl . -create /Groups/slopty-ci PrimaryGroupID 600");
        let user = at("if ! dscl . -read /Users/slopty-ci UniqueID");
        let named = at("  dscl . -create /Users/slopty-ci UniqueID 600");
        assert!(group < made && made < user && user < named, "{script}");
        let volume = at("daemon com.slopty.warm-volume\n");
        let waited = at("until [ -d /Volumes/slopty-ci/mac ]; do");
        let pboard = at("daemon com.slopty.warm-pboard\n");
        let mac = at("daemon com.slopty.warm-mac\n");
        let linux = at("daemon com.slopty.warm-linux\n");
        assert!(volume < waited && waited < pboard && pboard < mac && mac < linux, "{script}");
        for (_, plist) in &daemons {
            assert!(script.contains(&format!("<<'PLIST'\n{plist}PLIST\n")), "{plist} in it");
        }
        for needle in [
            "set -eu\n",
            "sudo sh /c/target/warm/admin.sh",
            "dscl . -search /Users UniqueID 600",
            "chmod 700 /Users/slopty-ci /Users/slopty-ci/tmp\n",
            "chown slopty-ci:slopty-ci /Library/Logs/slopty-warm\n",
            "if ! cmp -s \"$staged\" \"/Library/LaunchDaemons/$1.plist\"; then",
            "launchctl bootstrap system \"/Library/LaunchDaemons/$1.plist\"\n",
        ] {
            assert!(script.contains(needle), "{needle} in {script}");
        }
        assert!(!script.contains("/Volumes/slopty-ci/mac/com.slopty"), "no list from the volume");
    }
}

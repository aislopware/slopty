//! A check's app and simulator e2e cases in a macOS guest on this Mac, instead of on a runner
//! Mac.
//!
//! On a runner an e2e case spent two thirds of its time compiling: about two minutes of setup,
//! then a p50 of 501 s building the daemons and the app on three cores, for a p50 of 68 s of
//! tests (`.research/devloop-2026-10-17.md`). Here the build is incremental, in the lane's own
//! warm directory, and the tests run in a long-lived guest ([`GUEST`]) that shows nothing on this
//! Mac (`--no-graphics`) and touches none of its windows, input or grants, as the live lane's
//! guests do (`docs/TESTING.md`, "Live lane in a VM"). The guest runs the runners' very macOS
//! build (26.6.2, 25G83) on Apple's paravirtual GPU, as a runner does.
//!
//! - The lane's tree is the check's commit (`gate::snapshot_commit`), built into
//!   `target/local/<lane>/e2e-target` with the e2e features, apart from the check's own target dir,
//!   whose test-profile builds of the same binaries would replace these in `debug/`.
//! - The test binary goes over as a nextest archive, the binaries the suite spawns beside it, and
//!   the e2e crate's directory (its goldens) at the very path the tests were compiled with.
//! - One run at a time per guest, under a lock; a second lane waits and says so.
//! - Renders and diffs come back to `target/e2e/artifacts/`; accepted goldens come back into the
//!   tree, only those the run wrote; the log goes where `-E failed` reads it.
//! - The simulator case (`ios`) builds the app for the simulator here too, with this Mac's Xcode,
//!   and runs it in the guest's own simulator on the runners' iOS runtime ([`IOS_RUNTIME`]); the
//!   base holds that Xcode and runtime ([`make_base`]).

use std::fs::File;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path as Path, Utf8PathBuf as PathBuf};
use xshell::Shell;

use super::Goldens;

/// The guest that runs the e2e cases, a clone of [`BASE`].
pub const GUEST: &str = "slopty-26-e2e";

/// The base it is cloned from: [`ORIGIN`] with Xcode and the runners' iOS simulator runtime
/// ([`make_base`]).
const BASE: &str = "slopty-26-xcode";

/// The live lane's base (`cargo xtask vm create`), which [`BASE`] is made from: the runners'
/// macOS build.
const ORIGIN: &str = "slopty-26";

/// The iOS simulator runtime the runners' newest is (iOS 26.5, `macos-26` with Xcode 26.6, read on
/// run 39017057929); downloaded into the base by build, anonymously (`xcodebuild
/// -downloadPlatform`).
const IOS_RUNTIME: &str = "23F77";

/// The base's disk, in GB: macOS 12, Xcode 13, the runtime 8, and room for the runs.
const DISK_GB: &str = "90";

/// The guest's memory, in MB.
const MEMORY_MB: &str = "6144";

/// The account the guest logs in as.
const USER: &str = "admin";

/// The longest boot, to a command in the logged-in session: 40–50 s idle, 214 s with this Mac's
/// load at 50.
const BOOT: Duration = Duration::from_mins(6);

/// How many tests run at once in the guest: `SLOPTY_E2E_THREADS`, else three of its four
/// cores. The app's whole suite at HEAD 73366704 took 250 s one at a time and 97 s three at a
/// time, with the same four tests failing, and on a runner 193 s against 103 s with the same
/// nine (`.research/devloop-2026-10-17.md`). One (`SLOPTY_E2E_THREADS=1`) shows every line as
/// it comes.
fn threads() -> usize {
    std::env::var("SLOPTY_E2E_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(3).max(1)
}

/// How long the guest stays up with no run, after which it is stopped: it holds 5 GB of this
/// Mac's memory while up, whose swap the lanes' builds already fill, and boots again in 23 s on an
/// idle Mac.
const IDLE_STOP_S: u64 = 20 * 60;

/// Mark the guest used now, and leave a detached watcher that stops it once it has been idle for
/// [`IDLE_STOP_S`]: unused since, and no run holding its lock.
fn stop_when_idle(home: &Path) {
    use std::os::unix::process::CommandExt as _;
    let stamp = home.join("locks").join(format!("{GUEST}.used"));
    let lock = home.join("locks").join(format!("{GUEST}.lock"));
    if std::fs::write(&stamp, b"").is_err() {
        return;
    }
    let tart_home = home.join("tart");
    let script = format!(
        "sleep {IDLE_STOP_S}; now=$(date +%s); used=$(stat -f %m '{stamp}'); \
         [ $((now - used)) -ge {IDLE_STOP_S} ] || exit 0; \
         /usr/bin/lockf -t 0 '{lock}' /usr/bin/env TART_HOME='{tart_home}' tart stop {GUEST} --timeout 30"
    );
    let _watcher = Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

/// What one e2e run here asks.
pub struct Run<'a> {
    /// The check's name, for where its log goes.
    pub name: &'a str,
    /// The case's nextest filterset.
    pub filter: Option<&'a str>,
    /// What it does with the goldens.
    pub goldens: Goldens,
    /// The simulator case on this device, rather than the app's.
    pub ios: Option<crate::ios::SimKind>,
}

/// How a run here went.
pub struct Outcome {
    /// Whether every test passed (or the filter matched none in a filtered run).
    pub passed: bool,
    /// The build and the archive.
    pub build: Duration,
    /// The copy into the guest and the tests.
    pub tests: Duration,
    /// nextest's summary line, or why there is none.
    pub summary: String,
}

/// Where the guests live: `SLOPTY_VM_HOME`, else the external disk, as `xtask vm` has it.
fn home() -> PathBuf {
    std::env::var("SLOPTY_VM_HOME")
        .map_or_else(|_| PathBuf::from("/Volumes/Lacie/vms"), PathBuf::from)
}

/// Whether this Mac can run the e2e guest: tart, and the base to clone it from, or the live
/// lane's base to make that from.
pub fn available() -> bool {
    cfg!(target_os = "macos")
        && tart(&home()).arg("list").output().is_ok_and(|out| {
            out.status.success()
                && String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .any(|l| matches!(l.split_whitespace().nth(1), Some(BASE | ORIGIN)))
        })
}

/// Whether tart lists a guest `name`.
fn has_guest(home: &Path, name: &str) -> Result<bool> {
    Ok(tart_out(home, &["list"])?.lines().any(|l| l.split_whitespace().nth(1) == Some(name)))
}

fn tart(home: &Path) -> Command {
    let mut tart = Command::new("tart");
    tart.env("TART_HOME", home.join("tart"));
    tart
}

fn tart_out(home: &Path, args: &[&str]) -> Result<String> {
    let out = tart(home).args(args).stdin(Stdio::null()).output().context("tart")?;
    ensure!(
        out.status.success(),
        "tart {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// `ssh` and `rsync -e`'s options for the guest.
fn ssh_args(home: &Path) -> Vec<String> {
    let mut args = vec!["-i".to_owned(), home.join("ssh/id_ed25519").into_string()];
    for option in [
        "IdentitiesOnly=yes",
        "BatchMode=yes",
        "StrictHostKeyChecking=no",
        "UserKnownHostsFile=/dev/null",
        "LogLevel=ERROR",
        "ConnectTimeout=10",
        "ServerAliveInterval=10",
        "ControlMaster=auto",
        "ControlPersist=60",
    ] {
        args.extend(["-o".to_owned(), option.to_owned()]);
    }
    args.extend(["-o".to_owned(), format!("ControlPath={home}/ssh/cm-%C")]);
    args.extend(["-l".to_owned(), USER.to_owned()]);
    args
}

fn ssh(home: &Path, ip: &str, script: &str) -> Result<()> {
    let status = Command::new("ssh")
        .args(ssh_args(home))
        .arg(ip)
        .arg(script)
        .stdin(Stdio::null())
        .status()
        .context("ssh")?;
    ensure!(status.success(), "`{script}` on the guest: {status}");
    Ok(())
}

/// `script` over ssh on the guest; its output.
fn ssh_out(home: &Path, ip: &str, script: &str) -> Result<String> {
    let out = Command::new("ssh")
        .args(ssh_args(home))
        .arg(ip)
        .arg(script)
        .stdin(Stdio::null())
        .output()
        .context("ssh")?;
    ensure!(
        out.status.success(),
        "`{script}` on the guest: {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?)
}

/// Lanes' directories the guest keeps, the running lane's among them ([`prune`]): a lane's
/// tree, binaries and archive took 0.6–0.9 GB, and the e2e copies of its binaries 1.4 GB more.
const KEPT_LANES: usize = 4;

/// The free space a run needs on the guest's data volume before its copy, in GiB ([`prune`]): a
/// run writes at most a lane's directory, its binaries' copies and a simulator's data, 3 GB.
const FREE_GIB: u64 = 8;

/// The guest's free space in KiB, printed as `free <KiB>`.
const FREE: &str = "df -k /System/Volumes/Data | awk 'NR == 2 { print \"free\", $4 }'";

/// Make room for a run before its copy, or say plainly that there is none. What every run
/// leaves goes each time: the processes and stack roots of a stopped run, the e2e harness's
/// copies of the binaries, simulators `simctl` cannot use, and the `Deleting-<UDID>` directories
/// `simctl delete` leaves in the temporary directory and never removes, a run's simulator each
/// (109 of them filled the guest's 90 GB disk on 2026-10-17). Then the least recently used
/// lanes' directories beyond [`KEPT_LANES`], and more of them, oldest first, while the volume
/// has less than [`FREE_GIB`] free; the running lane's never.
fn prune(home: &Path, ip: &str, dir: &Path) -> Result<()> {
    let started = Instant::now();
    let target = dir.join("e2e-target");
    let lanes = dir.parent().context("a lane's directory sits in target/local")?;
    let lane = dir.file_name().context("a lane's directory has a name")?;
    let _unavailable = simctl(home, GUEST, &["delete", "unavailable"]);
    let listing = ssh_out(
        home,
        ip,
        &format!(
            "pkill -9 -f '{target}/debug/[s]lopty' ; \
             find \"$(getconf DARWIN_USER_TEMP_DIR)\" /private/tmp -maxdepth 1 \\( -name 'Deleting-*' \
             -o -name 'slopty-e2e-*' -o -name 'stub-claude-*' \\) -exec rm -rf {{}} + ; \
             sudo mkdir -p '{dir}' && sudo chown {USER} '{dir}' && touch '{dir}' && cd '{lanes}' && \
             stat -f '%m %N' * && {FREE}"
        ),
    )?;
    let (mut free, mut older) = lanes_by_use(&listing, lane)?;
    let mut dropped = Vec::new();
    while !older.is_empty() {
        // The running lane's is one of the kept.
        if older.len() < KEPT_LANES && free >= FREE_GIB << 20 {
            break;
        }
        let oldest = older.remove(0);
        let out = ssh_out(home, ip, &format!("sudo rm -rf '{lanes}/{oldest}' && {FREE}"))?;
        free = lanes_by_use(&out, lane)?.0;
        dropped.push(oldest);
    }
    if !dropped.is_empty() {
        println!(
            "  dropped {} from {GUEST}, least recently used; {:.1} GiB free ({:.1?})",
            dropped.join(", "),
            gib(free),
            started.elapsed()
        );
    }
    ensure!(
        free >= FREE_GIB << 20,
        "{GUEST} has {:.1} GiB free, short of the {FREE_GIB} GiB a run needs, with only lane \
         {lane}'s directory left to drop: see what fills it with `sudo du -xh -d 4 \
         /System/Volumes/Data | sort -h | tail -30` over `ssh admin@$(tart ip {GUEST})`",
        gib(free)
    );
    Ok(())
}

/// KiB in GiB, for a message.
#[expect(clippy::cast_precision_loss, reason = "a disk's size, shown to a tenth")]
fn gib(kib: u64) -> f64 {
    kib as f64 / f64::from(1 << 20)
}

/// From [`prune`]'s listing, `<mtime> <name>` per lane directory and `free <KiB>`: the free
/// KiB, and every lane but `lane`, least recently used first. A name a shell could misread is
/// left out, never dropped.
fn lanes_by_use(listing: &str, lane: &str) -> Result<(u64, Vec<String>)> {
    let mut free = None;
    let mut lanes = Vec::new();
    for line in listing.lines() {
        let Some((first, rest)) = line.trim().split_once(' ') else { continue };
        if first == "free" {
            free = Some(rest.trim().parse::<u64>().with_context(|| format!("df: {line}"))?);
            continue;
        }
        let Ok(mtime) = first.parse::<u64>() else { continue };
        let plain = !rest.is_empty()
            && rest != "."
            && rest != ".."
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
        if plain && rest != lane {
            lanes.push((mtime, rest.to_owned()));
        }
    }
    lanes.sort();
    let free = free.with_context(|| format!("no free space in the guest's listing: {listing}"))?;
    Ok((free, lanes.into_iter().map(|(_, name)| name).collect()))
}

/// `rsync -a` with `args`, over the guest's ssh.
fn rsync(home: &Path, args: &[&str], cwd: &Path) -> Result<()> {
    let ssh = format!("ssh {}", ssh_args(home).join(" "));
    let status = Command::new("rsync")
        .args(["-a", "-e", &ssh])
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .status()
        .context("rsync")?;
    ensure!(status.success(), "rsync {}: {status}", args.join(" "));
    Ok(())
}

/// The guest booted, cloned from the base first if it is not there; its address.
fn up(home: &Path) -> Result<String> {
    let listed = tart_out(home, &["list"])?;
    let state = listed
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some(GUEST))
        .map(|l| l.split_whitespace().last().unwrap_or("").to_owned());
    match state.as_deref() {
        Some("running") => return tart_out(home, &["ip", GUEST, "--wait", "60"]),
        Some(_) => {}
        None => {
            if !has_guest(home, BASE)? {
                make_base(home, BASE)?;
            }
            println!("  making the e2e guest {GUEST} from {BASE}");
            tart_out(home, &["clone", BASE, GUEST])?;
            // Three quarters of the base's: one app and its daemons, or one simulator, at a
            // time, while this Mac's lanes build beside it with their swap full.
            tart_out(home, &["set", GUEST, "--memory", MEMORY_MB])?;
        }
    }
    boot(home, GUEST, &[])
}

/// Boot `name` headless, as a process of its own; its address once a command runs in its
/// logged-in session.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling a guest")]
fn boot(home: &Path, name: &str, args: &[String]) -> Result<String> {
    let log = home.join("logs").join(format!("{name}.log"));
    let file = File::create(&log).with_context(|| format!("create {log}"))?;
    let started = Instant::now();
    let mut vm = {
        use std::os::unix::process::CommandExt as _;
        tart(home)
            .args(["run", name, "--no-graphics"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file)
            .process_group(0)
            .spawn()
            .context("tart run")?
    };
    loop {
        if let Some(status) = vm.try_wait()? {
            bail!("tart run {name} exited with {status}; see {log}");
        }
        let ready = tart(home)
            .args(["exec", name, "/usr/bin/true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ready {
            println!("  booted {name} in {:.0?}", started.elapsed());
            return tart_out(home, &["ip", name, "--wait", "60"]);
        }
        ensure!(started.elapsed() < BOOT, "{name} did not come up in {BOOT:?}; see {log}");
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Make [`BASE`] from [`ORIGIN`], once: a disk of [`DISK_GB`], this Mac's Xcode (the simulator
/// case builds the app here, with it, and the guest runs `simctl`), the runners' iOS runtime, the
/// two simulators booted once, no Spotlight indexing (it ran at a core after every boot), no crash
/// dialog (a window left over a crashed test's app). About
/// 28 minutes at this Mac's load: the runtime's 8.5 GB download and the simulators' first boots.
fn make_base(home: &Path, base: &str) -> Result<()> {
    println!("  making the e2e base {base} from {ORIGIN}: Xcode and iOS runtime {IOS_RUNTIME}");
    let making = format!("{base}-making");
    let drop = |home: &Path| -> Result<()> {
        if has_guest(home, &making)? {
            let _stopped = tart_out(home, &["stop", &making, "--timeout", "30"]);
            tart_out(home, &["delete", &making])?;
        }
        Ok(())
    };
    drop(home)?;
    tart_out(home, &["clone", ORIGIN, &making])?;
    if let Err(e) = fill_base(home, &making) {
        // Half made is not made: the next run starts over.
        let _dropped = drop(home);
        return Err(e);
    }
    tart_out(home, &["stop", &making, "--timeout", "60"])?;
    tart_out(home, &["rename", &making, base])?;
    println!("  made {base}");
    Ok(())
}

/// [`make_base`]'s work on the clone `making`, left running.
fn fill_base(home: &Path, making: &str) -> Result<()> {
    tart_out(home, &["set", making, "--disk-size", DISK_GB])?;
    let ip = boot(home, making, &[])?;
    let developer = Command::new("xcode-select").arg("-p").output().context("xcode-select")?;
    let developer = PathBuf::from(String::from_utf8(developer.stdout)?.trim());
    let xcode = developer
        .parent()
        .and_then(Path::parent)
        .context("xcode-select names no Xcode.app/Contents/Developer")?
        .to_path_buf();
    ssh(
        home,
        &ip,
        // `repairDisk` may grow the container itself, and `resizeContainer` then refuses the size
        // it already has: what counts is the size after.
        "yes | sudo diskutil repairDisk disk0 >/dev/null 2>&1; \
         sudo diskutil apfs resizeContainer disk0s2 0 >/dev/null 2>&1; \
         df -g / | awk 'NR == 2 { exit ($2 < 80) }'",
    )?;
    let to = format!("{ip}:/Users/{USER}/");
    rsync(home, &[xcode.as_str(), &to], home)?;
    let name = xcode.file_name().context("Xcode's name")?;
    ssh(
        home,
        &ip,
        &format!(
            "sudo rm -rf /Applications/Xcode.app && \
             sudo mv '/Users/{USER}/{name}' /Applications/Xcode.app && \
             sudo xcode-select -s /Applications/Xcode.app && sudo xcodebuild -license accept && \
             sudo xcodebuild -runFirstLaunch >/dev/null && \
             xcodebuild -downloadPlatform iOS -buildVersion {IOS_RUNTIME} >/dev/null && \
             sudo mdutil -a -i off >/dev/null && \
             defaults write com.apple.CrashReporter DialogType none"
        ),
    )?;
    // Each simulator's first boot migrates its data, 1–14 minutes: done once here, not in the
    // first run of every clone.
    for sim in [crate::ios::SimKind::Iphone, crate::ios::SimKind::Ipad] {
        pristine(home, making, sim)?;
    }
    Ok(())
}

/// What the guest's lock file says while a spare simulator boots under it ([`spare`]).
const SPARE_HOLDER: &str = "spare simulator";

/// The guest's lock, held for one run.
fn lock(home: &Path, lane: &str) -> Result<File> {
    let dir = home.join("locks");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{GUEST}.lock"));
    let file = File::options().create(true).truncate(false).write(true).open(&path)?;
    if file.try_lock().is_err() {
        let holder = std::fs::read_to_string(&path).unwrap_or_default();
        let holder = holder.trim();
        if holder == SPARE_HOLDER {
            println!("  waiting for the e2e guest: the next run's simulator is booting");
        } else {
            println!("  waiting for the e2e guest: {holder}'s run holds it");
        }
        file.lock().with_context(|| format!("lock {path}"))?;
    }
    std::fs::write(&path, lane)?;
    Ok(file)
}

/// nextest's config for the guest: the tree's, less its setup scripts, which run cargo (the
/// guest has none, and the binaries they would build are built).
fn guest_config(tree: &Path) -> Result<String> {
    let path = tree.join(".config/nextest.toml");
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
    let mut config: toml::Table = text.parse().with_context(|| format!("parse {path}"))?;
    let _scripts = config.remove("scripts");
    let _experimental = config.remove("experimental");
    if let Some(toml::Value::Table(profiles)) = config.get_mut("profile") {
        for (_name, profile) in profiles.iter_mut() {
            if let toml::Value::Table(profile) = profile {
                let _scripts = profile.remove("scripts");
            }
        }
    }
    Ok(toml::to_string(&config)?)
}

/// The executables a build left at the top of `debug`: what the suites spawn.
fn binaries(debug: &Path) -> Result<Vec<String>> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut names = Vec::new();
    for entry in std::fs::read_dir(debug).with_context(|| format!("read {debug}"))? {
        let entry = entry?;
        let meta = entry.metadata()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if meta.is_file() && meta.permissions().mode() & 0o111 != 0 && !name.contains('.') {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// Run the app's e2e case, or the simulator's, of commit `sha` in lane `lane`'s directory, in
/// the guest.
#[expect(clippy::too_many_lines, reason = "one run, step by step")]
pub fn e2e(root: &Path, lane: &str, sha: &str, run: &Run<'_>) -> Result<Outcome> {
    let started = Instant::now();
    let case = if run.ios.is_some() { "ios" } else { "app" };
    crate::prune::before_build();
    let dir = crate::local::dir(root, lane);
    let tree = crate::gate::snapshot_commit(root, &dir, sha)?;
    crate::local::target_by_config(&dir)?;
    let target = dir.join("e2e-target");
    let sh = Shell::new()?;
    sh.change_dir(&tree);
    sh.set_var("LIBGHOSTTY_VT_SYS_PREBUILT_DIR", root.join("target").join(crate::prune::PREBUILT));
    let target_arg = format!("--target-dir={target}");
    crate::e2e::build_app(&sh, &[&target_arg])?;
    // The simulator's app, built here with this Mac's Xcode into the same target dir.
    let bundle = match run.ios {
        Some(sim) => {
            crate::ios::preflight(&sh, false)?;
            let _target = sh.push_env("CARGO_TARGET_DIR", &target);
            Some(crate::ios::build_for_simulator(&sh, sim, "info")?)
        }
        None => None,
    };
    let stage = dir.join("guest");
    std::fs::create_dir_all(&stage)?;
    let archive = stage.join(format!("{case}.tar.zst"));
    let live = crate::e2e::LIVE;
    crate::tools::step(
        &format!("archive slopty-e2e {case}"),
        &xshell::cmd!(
            sh,
            "cargo nextest archive --cargo-profile dev {target_arg} -p slopty-e2e --test {case} --features {live} --archive-file {archive}"
        )
        .env(crate::gate::BINS_FRESH, "1"),
    )?;
    let config = stage.join("nextest.toml");
    std::fs::write(&config, guest_config(&tree)?)?;
    let built = started.elapsed();

    let home = home();
    let _held = lock(&home, lane)?;
    stop_when_idle(&home);
    let booting = Instant::now();
    let ip = up(&home)?;
    let booted = booting.elapsed();
    let copying = Instant::now();
    prune(&home, &ip, &dir)?;
    ssh(
        &home,
        &ip,
        &format!(
            "{{ [ ! -L '{target}/debug' ] || rm '{target}/debug'; }} && \
             mkdir -p '{target}/debug' '{tree}' '{stage}/x' && rm -rf '{tree}/target/e2e'"
        ),
    )?;
    let debug = target.join("debug");
    let bins = binaries(&debug)?;
    // Only the binaries that changed, each whole: rsync's delta transfer read and checksummed
    // both sides of every changed binary, 500 MB of them, and took 233 s with this Mac's lanes
    // building. Never `--inplace`: a file rewritten where the guest reads it was taken for a
    // corrupt archive, and a binary a killed run still maps keeps its old inode this way. Not a
    // shared directory: the guest's virtio-fs view kept a file the host had replaced by rename
    // as missing, as cargo replaces a binary and Xcode the app (2026-10-17).
    let mut args: Vec<&str> = vec!["--whole-file"];
    args.extend(bins.iter().map(String::as_str));
    let to = format!("{ip}:{debug}/");
    args.push(&to);
    rsync(&home, &args, &debug)?;
    let to = format!("{ip}:{tree}/");
    rsync(
        &home,
        &[
            "--relative",
            "--delete",
            "--exclude",
            "target/",
            "./crates",
            "./apps",
            "./.config",
            "./Cargo.toml",
            &to,
        ],
        &tree,
    )?;
    let nextest = which("cargo-nextest")?;
    let to = format!("{ip}:{stage}/");
    rsync(&home, &[archive.as_str(), nextest.as_str(), config.as_str(), &to], &stage)?;
    if let Some(bundle) = &bundle {
        let from = format!("{bundle}/");
        let to = format!("{ip}:{stage}/Slopty.app/");
        rsync(&home, &["--whole-file", "--delete", &from, &to], &stage)?;
    }
    println!(
        "  {GUEST} up in {booted:.1?}; {} binaries, the archive and sources copied in {:.1?}",
        bins.len(),
        copying.elapsed()
    );

    let data = tree.join("target/e2e");
    let accept = match run.goldens {
        Goldens::Compare => None,
        Goldens::Accept => Some("changed"),
        Goldens::AcceptAll => Some("all"),
        Goldens::Review => Some("review"),
    };
    // The simulator booted and the app installed in the session the tests run in.
    let simulator = if let Some(sim) = run.ios {
        Some(simulator(&home, sim, &stage.join("Slopty.app"))?)
    } else {
        // A booted simulator holds CoreAudio, and the app's audio player then takes tens of
        // seconds to open: no simulator stays up through the app's case.
        quiet(&home, None)?;
        None
    };
    let (filter, env) = match &simulator {
        Some(udid) => (
            crate::e2e::ios_filterset(run.filter),
            crate::e2e::ios_env(data.as_str(), debug.as_str(), accept, udid),
        ),
        None => (
            crate::e2e::app_filterset(run.filter),
            crate::e2e::app_env(data.as_str(), debug.as_str(), accept),
        ),
    };
    let filter = filter.unwrap_or_default();
    // One app in one simulator: its tests take turns.
    let output = match if simulator.is_some() { 1 } else { threads() } {
        1 => "--no-capture".to_owned(),
        // Passing tests' output too: their MEASURE lines are what a run reports.
        n => format!("--test-threads {n} --failure-output final --success-output final"),
    };
    let mut command = tart(&home);
    command.args(["exec", GUEST, "/usr/bin/env"]);
    for (key, value) in env {
        command.arg(format!("{key}={value}"));
    }
    command.arg(format!("INSTA_WORKSPACE_ROOT={tree}"));
    command.args([
        "/bin/sh",
        "-c",
        &format!(
            "mkdir -p '{data}/run' '{data}/artifacts' && cd '{tree}' && exec '{stage}/cargo-nextest' \
             nextest run --config-file '{stage}/nextest.toml' --archive-file '{archive}' \
             --extract-to '{stage}/x' --extract-overwrite --workspace-remap '{tree}' \
             --run-ignored only {output} --no-fail-fast -E \"$0\""
        ),
        &filter,
    ]);
    let logs = root.join("target/remote").join(run.name).join("logs/logs-run");
    if logs.exists() {
        std::fs::remove_dir_all(&logs)?;
    }
    std::fs::create_dir_all(&logs)?;
    let log = logs.join("e2e.log");
    println!(
        "▶ e2e {case} in {GUEST}{}",
        run.filter.map(|f| format!(" -E {f}")).unwrap_or_default()
    );
    let status = teed(command, &log);
    if let Some(sim) = run.ios {
        // The run's simulator goes, and the next run's is made ready while the guest idles.
        let _dropped = drop_devices(&home, &run_device(sim));
        spare(&home, sim);
    }
    let status = status?;
    let tests = copying.elapsed();
    stop_when_idle(&home);

    // Renders and diffs of changed frames, as a runner's come back.
    let renders = root.join("target/e2e/artifacts");
    if renders.exists() {
        std::fs::remove_dir_all(&renders)?;
    }
    std::fs::create_dir_all(&renders)?;
    let from = format!("{ip}:{data}/artifacts/");
    let _pulled = rsync(&home, &[&from, renders.as_str()], root);
    if accept.is_some_and(|a| a != "review") {
        let back = stage.join("golden-back");
        if back.exists() {
            std::fs::remove_dir_all(&back)?;
        }
        let from = format!("{ip}:{tree}/crates/slopty-e2e/golden/");
        rsync(&home, &[&from, back.as_str()], root)?;
        let applied = apply_goldens(
            &back,
            &tree.join("crates/slopty-e2e/golden"),
            &root.join("crates/slopty-e2e/golden"),
        )?;
        println!("  {applied} goldens the run wrote applied to the tree");
    }
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let summary = text
        .lines()
        .rev()
        .find(|l| l.contains("Summary ["))
        .map_or_else(|| "no summary: see the log".to_owned(), |l| l.trim().to_owned());
    let code = status.code();
    let passed =
        code == Some(0) || (run.filter.is_some() && code == Some(crate::e2e::NO_TESTS_RUN));
    if !passed {
        let failed = super::failed_tests(&text);
        for test in &failed {
            println!("        FAIL {test}");
        }
        println!("  every line: {log}; renders and diffs: {renders}");
    }
    Ok(Outcome { passed, build: built, tests, summary })
}

/// `simctl` in `guest`, through `tart exec`: in the session the tests run in, whose simulator
/// service is the one their `simctl launch` reaches.
fn simctl(home: &Path, guest: &str, args: &[&str]) -> Result<String> {
    let mut all = vec!["exec", guest, "xcrun", "simctl"];
    all.extend_from_slice(args);
    tart_out(home, &all)
}

/// `guest`'s pristine simulator for `sim`, shut down: never booted by a run, only cloned. Made
/// on the newest iOS runtime when there is none, and booted once, since a simulator's first boot
/// (its data migrated) takes 1–14 minutes; its UDID.
fn pristine(home: &Path, guest: &str, sim: crate::ios::SimKind) -> Result<String> {
    let name = sim.name();
    let devices = simctl(home, guest, &["list", "devices", "available", "-j"])?;
    if let Some(udid) = named_device(&devices, name)? {
        return Ok(udid);
    }
    let runtimes = simctl(home, guest, &["list", "runtimes", "-j"])?;
    let runtime = newest_ios(&runtimes)?.with_context(|| format!("no iOS runtime in {guest}"))?;
    println!("  creating simulator {name:?} on {runtime} in {guest}");
    let started = Instant::now();
    let udid = simctl(home, guest, &["create", name, sim.device_type(), &runtime])?;
    simctl(home, guest, &["boot", &udid])?;
    simctl(home, guest, &["bootstatus", &udid, "-b"])?;
    simctl(home, guest, &["shutdown", &udid])?;
    println!("  booted {name} once in {guest} ({:.1?})", started.elapsed());
    Ok(udid)
}

/// The name of a run's simulator, cloned from the pristine one for `sim`.
fn run_device(sim: crate::ios::SimKind) -> String {
    format!("{} run", sim.name())
}

/// The name of the next run's simulator for `sim`, cloned and booted ahead ([`spare`]).
fn spare_device(sim: crate::ios::SimKind) -> String {
    format!("{} spare", sim.name())
}

/// Clone and boot the next run's simulator for `sim` while the guest is idle, unless one is
/// there: a process of its own, left to run; at most one spare, for the device used last. A clone's
/// boot took 34–96 s idle and 445 s with this Mac's lanes building, all of it on a run's way
/// before; the next run takes the spare as it is, booted or still booting, and clones its own only
/// when there is none. Nothing runs in a spare before a run takes it, so it is as fresh as one
/// cloned for the run.
fn spare(home: &Path, sim: crate::ios::SimKind) {
    use std::os::unix::process::CommandExt as _;
    let name = spare_device(sim);
    let ready = || -> Result<Option<String>> {
        let devices = simctl(home, GUEST, &["list", "devices", "-j"])?;
        if named_device(&devices, &name)?.is_some() {
            return Ok(None);
        }
        Ok(Some(pristine(home, GUEST, sim)?))
    };
    let Ok(Some(source)) = ready() else { return };
    let script = format!(
        "u=$(xcrun simctl clone {source} '{name}') && xcrun simctl boot \"$u\"; \
         xcrun simctl bootstatus \"$u\" -b >/dev/null"
    );
    // Under the guest's lock, taken once this run lets go of it (or given up after a minute): a
    // spare booting beside another run's tests took its cores, and one booting into an app run
    // its CoreAudio. A run that comes meanwhile waits for it, then takes it or drops it.
    let lock = home.join("locks").join(format!("{GUEST}.lock"));
    let tart_home = home.join("tart");
    let held = format!(
        "printf '{SPARE_HOLDER}' > '{lock}'; exec /usr/bin/env TART_HOME='{tart_home}' \
         tart exec {GUEST} /bin/sh -c \"$0\""
    );
    let _spare = Command::new("/usr/bin/lockf")
        .args(["-t", "60", lock.as_str(), "/bin/sh", "-c", &held, &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

/// A simulator for one run in [`GUEST`], booted with `app` installed; its UDID. A clone of the
/// pristine one, so every run starts as a runner's freshly made simulator does: no app data,
/// notifications not yet asked, the keyboard, appearance and pasteboard as made (a simulator
/// the runs kept kept the app's notification grant, and every iOS frame showed it). A clone of a
/// migrated simulator boots in a reboot's time.
fn simulator(home: &Path, sim: crate::ios::SimKind, app: &Path) -> Result<String> {
    let started = Instant::now();
    let name = run_device(sim);
    let devices = simctl(home, GUEST, &["list", "devices", "-j"])?;
    let spare = named_device(&devices, &spare_device(sim))?;
    // Only the run's simulator is up: two beside the tests left the guest 64 MB free, in swap,
    // and the app hung 15 s at a time.
    quiet(home, spare.as_deref())?;
    let (udid, how) = if let Some(udid) = spare {
        simctl(home, GUEST, &["rename", &udid, &name])?;
        (udid, "took the spare")
    } else {
        let source = pristine(home, GUEST, sim)?;
        let udid = simctl(home, GUEST, &["clone", &source, &name])?;
        let _booting = simctl(home, GUEST, &["boot", &udid]);
        (udid, "booted a fresh")
    };
    // Boots it if it is not yet (a spare its process has not booted yet), else waits for it.
    simctl(home, GUEST, &["bootstatus", &udid, "-b"])?;
    simctl(home, GUEST, &["install", &udid, app.as_str()])?;
    println!("  {how} {} in {GUEST} and installed the app ({:.1?})", sim.name(), started.elapsed());
    Ok(udid)
}

/// Every simulator in [`GUEST`] down but `keep`: runs' and spares' deleted, any other shut down.
fn quiet(home: &Path, keep: Option<&str>) -> Result<()> {
    let listed = simctl(home, GUEST, &["list", "devices", "-j"])?;
    let pristine = [crate::ios::SimKind::Iphone.name(), crate::ios::SimKind::Ipad.name()];
    for device in devices(&listed)? {
        if Some(device.udid.as_str()) == keep {
            continue;
        }
        if pristine.contains(&device.name.as_str()) {
            if device.state != "Shutdown" {
                simctl(home, GUEST, &["shutdown", &device.udid])?;
            }
        } else if device.name.ends_with(" run") || device.name.ends_with(" spare") {
            let _shut = simctl(home, GUEST, &["shutdown", &device.udid]);
            simctl(home, GUEST, &["delete", &device.udid])?;
        } else if device.state != "Shutdown" {
            simctl(home, GUEST, &["shutdown", &device.udid])?;
        }
    }
    Ok(())
}

/// Shut down and delete every simulator named `name` (a run's, a stopped run's, a spare).
fn drop_devices(home: &Path, name: &str) -> Result<()> {
    let devices = simctl(home, GUEST, &["list", "devices", "-j"])?;
    for udid in named_devices(&devices, name)? {
        let _shut = simctl(home, GUEST, &["shutdown", &udid]);
        simctl(home, GUEST, &["delete", &udid])?;
    }
    Ok(())
}

/// The UDID of the first simulator named `name`, from `simctl list devices [available] -j`.
fn named_device(json: &str, name: &str) -> Result<Option<String>> {
    Ok(named_devices(json, name)?.into_iter().next())
}

/// The UDIDs of every simulator named `name`, from `simctl list devices [available] -j`.
fn named_devices(json: &str, name: &str) -> Result<Vec<String>> {
    Ok(devices(json)?.into_iter().filter(|d| d.name == name).map(|d| d.udid).collect())
}

/// A simulator as `simctl list devices -j` lists it.
#[derive(serde::Deserialize)]
struct Device {
    name: String,
    udid: String,
    /// `Shutdown`, `Booted`, `Booting`, …
    #[serde(default)]
    state: String,
}

/// Every simulator `simctl list devices [available] -j` lists.
fn devices(json: &str) -> Result<Vec<Device>> {
    #[derive(serde::Deserialize)]
    struct List {
        devices: std::collections::BTreeMap<String, Vec<Device>>,
    }
    let list: List = serde_json::from_str(json).context("simctl's device list")?;
    Ok(list.devices.into_values().flatten().collect())
}

/// The newest available iOS runtime's identifier, from `simctl list runtimes -j`.
fn newest_ios(json: &str) -> Result<Option<String>> {
    #[derive(serde::Deserialize)]
    struct List {
        runtimes: Vec<Runtime>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Runtime {
        identifier: String,
        version: String,
        platform: String,
        is_available: bool,
    }
    let version = |v: &str| -> Vec<u32> { v.split('.').map_while(|n| n.parse().ok()).collect() };
    let list: List = serde_json::from_str(json).context("simctl's runtime list")?;
    Ok(list
        .runtimes
        .into_iter()
        .filter(|r| r.platform == "iOS" && r.is_available)
        .max_by(|a, b| version(&a.version).cmp(&version(&b.version)))
        .map(|r| r.identifier))
}

/// Copy into `tree_goldens` each file of `back` (the guest's goldens after the run) whose bytes
/// differ from `snapshot`'s (what the run started from): the goldens the run wrote, and none of
/// another lane's edits in the tree. How many.
fn apply_goldens(back: &Path, snapshot: &Path, tree_goldens: &Path) -> Result<usize> {
    let mut applied = 0_usize;
    for entry in walk(back)? {
        let below = entry.strip_prefix(back).context("a golden outside its dir")?;
        let was = std::fs::read(snapshot.join(below)).ok();
        let now = std::fs::read(&entry)?;
        if was.as_deref() != Some(now.as_slice()) {
            let to = tree_goldens.join(below);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&to, &now)?;
            applied = applied.saturating_add(1);
        }
    }
    Ok(applied)
}

/// Every file under `dir`.
fn walk(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).with_context(|| format!("read {dir}"))? {
            let path = PathBuf::try_from(entry?.path())?;
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// `name`'s path on `PATH`.
fn which(name: &str) -> Result<PathBuf> {
    let out = Command::new("which").arg(name).output().context("which")?;
    ensure!(out.status.success(), "{name} is not on PATH");
    Ok(PathBuf::from(String::from_utf8(out.stdout)?.trim()))
}

/// Run `command`, its output into `log` and, less the lines of what passed, onto this terminal;
/// how it ended.
fn teed(mut command: Command, log: &Path) -> Result<std::process::ExitStatus> {
    use std::io::{BufRead as _, Write as _};
    let mut file = File::create(log).with_context(|| format!("create {log}"))?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("tart exec")?;
    let stderr = child.stderr.take().context("stderr")?;
    let (tx, lines) = std::sync::mpsc::channel::<String>();
    let err_tx = tx.clone();
    let err = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
            if err_tx.send(line).is_err() {
                break;
            }
        }
    });
    let stdout = child.stdout.take().context("stdout")?;
    let out = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    for line in lines {
        writeln!(file, "{line}")?;
        let trimmed = line.trim_start();
        let shown = ["FAIL [", "TIMEOUT [", "Summary [", "Starting ", "SIG", "error:"]
            .iter()
            .any(|p| trimmed.starts_with(p));
        if shown {
            println!("{line}");
        }
    }
    let joined = (out.join(), err.join());
    ensure!(joined.0.is_ok() && joined.1.is_ok(), "a reader thread panicked");
    Ok(child.wait()?)
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    /// The base is made from the live lane's: Xcode, the runners' iOS runtime, both simulators.
    /// Live: 28 minutes and an 8.5 GB download, past nextest's deadline for xtask (`cargo test -p
    /// xtask --lib -- --ignored a_base_is_made`); it makes `slopty-26-xcode-test` and deletes it.
    #[test]
    #[ignore = "live: makes a macOS guest"]
    fn a_base_is_made_from_the_live_lane_s() {
        let home = super::home();
        let base = "slopty-26-xcode-test";
        if super::has_guest(&home, base).expect("tart") {
            super::tart_out(&home, &["delete", base]).expect("delete the last test's");
        }
        super::make_base(&home, base).expect("made");
        let ip = super::boot(&home, base, &[]).expect("boots");
        let listed = super::tart_out(
            &home,
            &["exec", base, "xcrun", "simctl", "list", "devices", "available", "-j"],
        )
        .expect("simctl");
        for sim in [crate::ios::SimKind::Iphone, crate::ios::SimKind::Ipad] {
            assert!(super::named_device(&listed, sim.name()).expect("parses").is_some(), "{ip}");
        }
        let runtimes =
            super::tart_out(&home, &["exec", base, "xcrun", "simctl", "runtime", "list"])
                .expect("runtimes");
        assert!(runtimes.contains(super::IOS_RUNTIME), "{runtimes}");
        super::tart_out(&home, &["stop", base, "--timeout", "60"]).expect("stop");
        super::tart_out(&home, &["delete", base]).expect("delete");
    }

    /// The guest's simulator is found by name across runtimes, and one is made on the newest
    /// available iOS runtime, never a newer unavailable one or another platform's.
    #[test]
    fn the_simulator_is_found_by_name_or_made_on_the_newest_ios() {
        let devices = r#"{"devices": {
            "com.apple.CoreSimulator.SimRuntime.iOS-26-5": [
                {"name": "Slopty iPad", "udid": "76FFF7A1-9F82-4F1D-8E6C-2FEDA6CCF705"},
                {"name": "Slopty iPhone", "udid": "EAA99FB6-E2BE-4FFE-B921-2413AC6653E4"}
            ]}}"#;
        assert_eq!(
            super::named_device(devices, "Slopty iPhone").expect("parses").as_deref(),
            Some("EAA99FB6-E2BE-4FFE-B921-2413AC6653E4")
        );
        assert_eq!(super::named_device(devices, "Slopty Watch").expect("parses"), None);
        let runtimes = r#"{"runtimes": [
            {"identifier": "ios-26-2", "version": "26.2", "platform": "iOS", "isAvailable": true},
            {"identifier": "ios-26-10", "version": "26.10", "platform": "iOS", "isAvailable": true},
            {"identifier": "ios-27", "version": "27.0", "platform": "iOS", "isAvailable": false},
            {"identifier": "tvos-28", "version": "28.0", "platform": "tvOS", "isAvailable": true}
        ]}"#;
        assert_eq!(super::newest_ios(runtimes).expect("parses").as_deref(), Some("ios-26-10"));
    }

    /// The guest's lanes come least recently used first, the running lane's and any name a
    /// shell could misread left out; the free space is read, and a listing without it refused.
    #[test]
    fn the_guest_s_lanes_go_least_recently_used_first() {
        let listing = "1760695200 E2\n1760688000 DEVLOOP\n1760702400 B\n1760690000 C3\n\
                       1760691000 a'b\n1760692000 ..\n  free 22950404\n";
        let (free, older) = super::lanes_by_use(listing, "B").expect("parses");
        assert_eq!(free, 22_950_404);
        assert_eq!(older, ["DEVLOOP", "C3", "E2"]);
        let _no_free = super::lanes_by_use("1760695200 E2\n", "B").unwrap_err();
        let _not_a_number = super::lanes_by_use("free lots\n", "B").unwrap_err();
    }

    /// Only the goldens whose bytes the run changed, or wrote anew, reach the tree; one the
    /// run left as it found it keeps whatever the tree holds (another lane's edit).
    #[test]
    fn only_the_goldens_a_run_wrote_come_back() {
        let dir = std::env::temp_dir().join(format!("xtask-guest-goldens-{}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&dir);
        let dir = Utf8PathBuf::from_path_buf(dir).expect("utf8");
        let (back, snapshot, tree) = (dir.join("back"), dir.join("snapshot"), dir.join("tree"));
        for d in [&back, &snapshot, &tree] {
            std::fs::create_dir_all(d.join("ipad")).expect("dirs");
        }
        std::fs::write(snapshot.join("same.png"), "a").expect("write");
        std::fs::write(back.join("same.png"), "a").expect("write");
        std::fs::write(tree.join("same.png"), "edited by another lane").expect("write");
        std::fs::write(snapshot.join("ipad/moved.txt"), "old").expect("write");
        std::fs::write(back.join("ipad/moved.txt"), "new").expect("write");
        std::fs::write(back.join("fresh.png"), "made").expect("write");
        assert_eq!(super::apply_goldens(&back, &snapshot, &tree).expect("applied"), 2);
        let read = |p: &str| std::fs::read_to_string(tree.join(p)).expect("read");
        assert_eq!(read("same.png"), "edited by another lane");
        assert_eq!(read("ipad/moved.txt"), "new");
        assert_eq!(read("fresh.png"), "made");
        let _gone = std::fs::remove_dir_all(&dir);
    }
}

//! A check's app and simulator e2e cases in a macOS guest on this Mac, instead of on a runner
//! Mac.
//!
//! On a runner an e2e case spent two thirds of its time compiling: about two minutes of setup,
//! then a p50 of 501 s building the daemons and the app on three cores, for a p50 of 68 s of
//! tests (`.research/devloop-2026-10-17.md`). Here the build is incremental, in the lane's own
//! warm directory, and the tests run in one of two long-lived guests ([`crate::vm::E2E_GUESTS`])
//! that show nothing on this Mac (`--no-graphics`), play nothing on its speakers ([`QUIET`]) and
//! touch none of its windows, input or grants, as the live lane's guests do (`docs/TESTING.md`,
//! "Live lane in a VM"). The guests run macOS 27.0 (26A428), Cirrus Labs' image, beside the
//! runners' 27.0.1 (26A434, `xcode-27`), whose kernel and GPU kexts are the same builds, on
//! Apple's paravirtual GPU, as a runner's. Both are clones of one base, so they match.
//!
//! - The lane's tree is the check's commit (`gate::snapshot_commit`), built into
//!   `target/local/<lane>/e2e-target` with the e2e features, apart from the check's own target dir,
//!   whose test-profile builds of the same binaries would replace these in `debug/`.
//! - The test binary goes over as a nextest archive, the binaries the suite spawns beside it, and
//!   the e2e crate's directory (its goldens) at the very path the tests were compiled with.
//! - Two lanes' runs at once, one in each guest, each under its guest's lock ([`hold`]): a run
//!   takes the guest its lane used last if it is free, else a running one, else the other; a third
//!   lane waits for whichever frees first and says who holds them. Each guest has its own
//!   simulators and data; nothing a run keeps on this Mac is shared with another lane's run.
//! - Renders and diffs come back to `target/remote/<name>/artifacts/`, the check's own; accepted
//!   goldens come back into the tree, only those the run wrote; the log goes where `-E failed`
//!   reads it.
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

/// The base the e2e guests ([`crate::vm::E2E_GUESTS`]) are cloned from: [`ORIGIN`] with Xcode
/// and the runners' iOS simulator runtime ([`make_base`]).
const BASE: &str = "slopty-27-xcode";

/// The live lane's base (`cargo xtask vm create`), which [`BASE`] is made from: Cirrus Labs'
/// macOS 27.0.
const ORIGIN: &str = "slopty-27";

/// The iOS simulator runtime the runners' simulators run on (iOS 27.0: on the image of
/// 2026-10-29 the only other is 27.1, which takes the iPhone Duo alone, so `ios::choose_simulator`
/// makes neither kind on it there); downloaded into the base by build, anonymously
/// (`xcodebuild -downloadPlatform`).
const IOS_RUNTIME: &str = "24A434";

/// The base's disk, in GB: macOS 12, Xcode 13, the runtime 8, and room for the runs.
const DISK_GB: &str = "90";

/// The guest's memory, in MB: an iOS run's simulator and app filled 6, 8 and 10 GB alike, with
/// the compressor at 1.6–1.8 GB and about 80 MB unused (`docs/decisions/tooling.md`, "6 GB
/// stays, measured against 8 and 10 GB"). Set on every boot, so a guest made before a change
/// takes it.
const MEMORY_MB: &str = "6144";

/// The display as the runners' image leaves it (`images/macos/scripts/build/configure-system.sh`
/// in `actions/runner-images`, "Disable graphic effects in System"): motion and transparency
/// reduced. The app reads both from the system (under less transparency a message's light and a
/// frosted surface stay flat), and a golden holds in the guest and on a runner alike only when
/// both draw the same.
const RUNNERS_DISPLAY: &str = "defaults write com.apple.universalaccess reduceMotion -bool true && \
     defaults write com.apple.universalaccess reduceTransparency -bool true";

/// The guest's sound output muted: tart passes a guest's audio to this Mac's speakers, and the
/// app's chimes (`afplay`, three apps at a time) played there, where the person hears this Mac
/// over Parsec. The device stays, so the app's audio player opens as on a runner.
const QUIET: &str = "osascript -e 'set volume output muted true'";

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

/// How long the guest stays up with no run, after which it is stopped: it holds up to 6 GB of
/// this Mac's memory while up, whose swap the lanes' builds already fill, and boots again in 23 s
/// on an idle Mac.
const IDLE_STOP_S: u64 = 20 * 60;

/// Mark the guest used now, and leave a detached watcher that stops it once it has been idle for
/// [`IDLE_STOP_S`]: unused since, and no run holding its lock. `lockf -k`, since without it
/// `lockf` removes the lock file as it lets go, under any process waiting on that file.
fn stop_when_idle(home: &Path, slot: &crate::vm::Slot) {
    use std::os::unix::process::CommandExt as _;
    let guest = slot.guest();
    let stamp = home.join("locks").join(format!("{guest}.used"));
    let lock = crate::vm::slot_lock(home, guest);
    if std::fs::write(&stamp, b"").is_err() {
        return;
    }
    let tart_home = home.join("tart");
    let script = format!(
        "sleep {IDLE_STOP_S}; now=$(date +%s); used=$(stat -f %m '{stamp}'); \
         [ $((now - used)) -ge {IDLE_STOP_S} ] || exit 0; \
         /usr/bin/lockf -k -t 0 '{lock}' /usr/bin/env TART_HOME='{tart_home}' tart stop {guest} --timeout 30"
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
    /// How many times the case runs in this one session.
    pub repeat: Repeat,
}

/// How many times a run's case runs, in one guest session: booted and copied once, each time
/// on a fresh simulator, its failures' artifacts kept apart ([`e2e`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    /// This many times, every one, to see how often it fails (`--repeat`; one is a plain run).
    Times(u32),
    /// Up to this many times, stopping at the first failure (`--until-fail`): whether a fix
    /// holds.
    UntilFail(u32),
}

impl Repeat {
    /// The most times the case runs.
    const fn most(self) -> u32 {
        match self {
            Self::Times(n) | Self::UntilFail(n) => n,
        }
    }
}

/// The confidence a repeated run reports at, and [`clean_runs_for`] reaches.
const CONFIDENCE: f64 = 0.95;

/// The clean runs in a row after which a flake that failed `rate` of its runs (0 < rate < 1)
/// is gone at [`CONFIDENCE`]: were it still there, that many would all pass with a chance of at
/// most 5 %, `(1 - rate)^n <= 0.05`. A flake failing 1 run in 5 takes 14.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a small positive count, rounded up"
)]
fn clean_runs_for(rate: f64) -> u32 {
    (1.0 - CONFIDENCE).log(1.0 - rate).ceil().max(1.0) as u32
}

/// What `failed` failures in `ran` runs of `most` say, for [`Repeat`]'s last line.
fn repeat_verdict(repeat: Repeat, failed: u32, ran: u32) -> String {
    let most = repeat.most();
    if failed == 0 {
        // The rate a flake would need to fail none of `ran` runs with a 5 % chance.
        let bound = 1.0 - (1.0 - CONFIDENCE).powf(1.0 / f64::from(ran));
        return format!(
            "0 of {ran} failed: a flake failing 1 run in {:.1} or more often would have shown, \
             at 95 %; one seen failing 1 in 5 is fixed after {} clean runs in a row",
            1.0 / bound,
            clean_runs_for(0.2)
        );
    }
    if let Repeat::UntilFail(_) = repeat {
        if ran == 1 {
            return format!(
                "failed on the first run, of up to {most}: `--repeat <N>` shows how often it fails"
            );
        }
        let needed = clean_runs_for(1.0 / f64::from(ran));
        return format!(
            "failed at run {ran} of up to {most}, about 1 in {ran}: a fix holds at 95 % after \
             {needed} clean runs in a row (`--until-fail {needed}`)"
        );
    }
    if failed == ran {
        return format!("{failed} of {ran} failed: it fails every run, a bug rather than a flake");
    }
    let rate = f64::from(failed) / f64::from(ran);
    let needed = clean_runs_for(rate);
    format!(
        "{failed} of {ran} failed, 1 in {:.1}: a fix holds at 95 % after {needed} clean runs in a \
         row (`--until-fail {needed}`)",
        1.0 / rate
    )
}

/// How a run here went.
pub struct Outcome {
    /// Whether every test passed (or the filter matched none in a filtered run).
    pub passed: bool,
    /// The e2e guest it ran in.
    pub guest: &'static str,
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
fn prune(home: &Path, guest: &str, ip: &str, dir: &Path) -> Result<()> {
    let started = Instant::now();
    let target = dir.join("e2e-target");
    let lanes = dir.parent().context("a lane's directory sits in target/local")?;
    let lane = dir.file_name().context("a lane's directory has a name")?;
    let _unavailable = simctl(home, guest, &["delete", "unavailable"]);
    let listing = ssh_out(
        home,
        ip,
        &format!(
            "pkill -9 -f '{target}/debug/[s]lopty|slopty-e2e-bin-[0-9a-f]+/[s]lopty' ; \
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
            "  dropped {} from {guest}, least recently used; {:.1} GiB free ({:.1?})",
            dropped.join(", "),
            gib(free),
            started.elapsed()
        );
    }
    ensure!(
        free >= FREE_GIB << 20,
        "{guest} has {:.1} GiB free, short of the {FREE_GIB} GiB a run needs, with only lane \
         {lane}'s directory left to drop: see what fills it with `sudo du -xh -d 4 \
         /System/Volumes/Data | sort -h | tail -30` over `ssh admin@$(tart ip {guest})`",
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

/// `guest` booted, cloned from the base first if it is not there; its address. Every guest is
/// a clone of the one base, set up the same way on every boot, so the pool's two match.
fn up(home: &Path, guest: &str) -> Result<String> {
    match state(home, guest)?.as_deref() {
        Some("running") => return tart_out(home, &["ip", guest, "--wait", "60"]),
        Some(_) => {}
        None => {
            if !has_guest(home, BASE)? {
                make_base(home, BASE)?;
            }
            println!("  making the e2e guest {guest} from {BASE}");
            tart_out(home, &["clone", BASE, guest])?;
        }
    }
    tart_out(home, &["set", guest, "--memory", MEMORY_MB])?;
    let ip = boot(home, guest, &[])?;
    // On every boot, so a guest cloned from a base made before takes them.
    ssh(home, &ip, crate::vm::NO_UPDATES)?;
    ssh(home, &ip, RUNNERS_DISPLAY)?;
    exec(home, guest, QUIET)?;
    Ok(ip)
}

/// `guest`'s state as `tart list` says it (`running`, `stopped`, …), `None` when there is none.
fn state(home: &Path, guest: &str) -> Result<Option<String>> {
    Ok(tart_out(home, &["list"])?
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some(guest))
        .map(|l| l.split_whitespace().last().unwrap_or("").to_owned()))
}

/// `script` run by `sh` in `guest`'s logged-in session (`tart exec`).
fn exec(home: &Path, guest: &str, script: &str) -> Result<()> {
    tart_out(home, &["exec", guest, "/bin/sh", "-c", script]).map(|_said| ())
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
/// two simulators booted once and settled, no Spotlight indexing (it ran at a core after every
/// boot), no crash dialog (a window left over a crashed test's app). About 28 minutes at this
/// Mac's load for the runtime's 8.5 GB download and the simulators' first boots, and up to 20
/// more for each simulator to settle.
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
        // it already has: what counts is the size after. macOS 27 keeps a 5.4 GB Recovery
        // container after it, so 90 GB leaves the system 78.
        "yes | sudo diskutil repairDisk disk0 >/dev/null 2>&1; \
         sudo diskutil apfs resizeContainer disk0s2 0 >/dev/null 2>&1; \
         df -g / | awk 'NR == 2 { exit ($2 < 75) }'",
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
    // Each simulator's first boot migrates its data, 1–14 minutes, and its daemons' work after
    // takes up to 20: done once here, not in the first run of every clone.
    for sim in [crate::ios::SimKind::Iphone, crate::ios::SimKind::Ipad] {
        pristine(home, making, sim)?;
    }
    Ok(())
}

/// What the guest's lock file says while a spare simulator boots under it ([`spare`]).
const SPARE_HOLDER: &str = "spare simulator";

/// How often a run that waits for an e2e guest looks again.
const POOL_WAIT: Duration = Duration::from_secs(2);

/// An e2e guest's slot, held for one run of lane `lane` (whose directory is `dir`): the guest
/// the lane used last when it is free (its copies of the lane's binaries are there), else a free
/// one that is running (no boot), else any free one; with both taken, whichever frees first.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; waiting a turn")]
fn hold(home: &Path, lane: &str, dir: &Path) -> Result<crate::vm::Slot> {
    std::fs::create_dir_all(home.join("locks"))?;
    let last = dir.join("e2e-guest");
    let order = preferred(
        std::fs::read_to_string(&last).ok().as_deref().map(str::trim),
        &tart_out(home, &["list"])?,
    );
    let started = Instant::now();
    let mut told = false;
    loop {
        for &guest in &order {
            if let Some(slot) = crate::vm::try_slot(home, guest, lane)? {
                if told {
                    println!("  waited {:.0?} for {}", started.elapsed(), slot.guest());
                }
                std::fs::write(&last, slot.guest())?;
                return Ok(slot);
            }
        }
        if !told {
            let held: Vec<String> = crate::vm::E2E_GUESTS
                .map(|guest| {
                    let holder = crate::vm::slot_holder(home, guest);
                    let holder = if holder == SPARE_HOLDER {
                        "the next run's simulator, booting".to_owned()
                    } else {
                        format!("{holder}'s run")
                    };
                    format!("{guest} by {holder}")
                })
                .into();
            println!("  waiting for an e2e guest: {}", held.join(", "));
            told = true;
        }
        std::thread::sleep(POOL_WAIT);
    }
}

/// The order a run tries the e2e guests in: the one named `last` (the lane's last), then those
/// `tart list`'s `listed` says run, then the rest, each group in the pool's order.
fn preferred(last: Option<&str>, listed: &str) -> Vec<&'static str> {
    let running = |guest: &str| {
        listed.lines().any(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            words.get(1) == Some(&guest) && words.last() == Some(&"running")
        })
    };
    let mut order = crate::vm::E2E_GUESTS.to_vec();
    order.sort_by_key(|&guest| (Some(guest) != last, !running(guest)));
    order
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
    crate::local::seed(&dir, "e2e-target");
    let target = dir.join("e2e-target");
    let sh = Shell::new()?;
    sh.change_dir(&tree);
    sh.set_var("LIBGHOSTTY_VT_SYS_PREBUILT_DIR", root.join("target").join(crate::prune::PREBUILT));
    crate::tools::use_zig_for(&sh, &tree)?;
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
    let slot = hold(&home, lane, &dir)?;
    let guest = slot.guest();
    stop_when_idle(&home, &slot);
    let booting = Instant::now();
    let ip = up(&home, guest)?;
    let booted = booting.elapsed();
    let copying = Instant::now();
    prune(&home, guest, &ip, &dir)?;
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
        "  {guest} up in {booted:.1?}; {} binaries, the archive and sources copied in {:.1?}",
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
    let most = run.repeat.most();
    ensure!(most >= 1, "a case runs at least once");
    ensure!(
        most == 1 || matches!(run.goldens, Goldens::Compare | Goldens::Review),
        "a repeated run compares its frames or reviews them; an accept takes one run"
    );
    if run.ios.is_none() {
        // A booted simulator holds CoreAudio, and the app's audio player then takes tens of
        // seconds to open: no simulator stays up through the app's case.
        quiet(&home, guest, None)?;
    }
    let logs = root.join("target/remote").join(run.name).join("logs/logs-run");
    if logs.exists() {
        std::fs::remove_dir_all(&logs)?;
    }
    std::fs::create_dir_all(&logs)?;
    let log = logs.join("e2e.log");
    // Each run's log, and a failed run's whole data directory and kept stack roots.
    let kept = root.join("target/remote").join(run.name).join("repeat");
    if most > 1 {
        if kept.exists() {
            std::fs::remove_dir_all(&kept)?;
        }
        std::fs::create_dir_all(&kept)?;
    }
    println!(
        "▶ e2e {case} in {guest}{}{}",
        run.filter.map(|f| format!(" -E {f}")).unwrap_or_default(),
        match run.repeat {
            Repeat::Times(1) => String::new(),
            Repeat::Times(n) => format!(", {n} times"),
            Repeat::UntilFail(n) => format!(", up to {n} times until one fails"),
        }
    );
    let mut status = None;
    let (mut ran, mut failed, mut first_failed) = (0_u32, 0_u32, None);
    for at in 1..=most {
        if at > 1 {
            // This run starts as a run alone does: the last run's daemons gone, from `debug`
            // or from the harness's copies of the binaries, and its data, stack roots and
            // frames, kept if it failed, cleared. The copies stay.
            ssh(
                &home,
                &ip,
                &format!(
                    "pkill -9 -f '{target}/debug/[s]lopty|slopty-e2e-bin-[0-9a-f]+/[s]lopty' ; rm -rf '{data}' ; find \"$(getconf DARWIN_USER_TEMP_DIR)\" /private/tmp -maxdepth 1 \
                     -name 'slopty-e2e-*' ! -name 'slopty-e2e-bin-*' -exec rm -rf {{}} +"
                ),
            )?;
        }
        // The simulator booted and the app installed in the session the tests run in: a
        // fresh clone each run, as a run alone starts.
        let simulator = match run.ios {
            Some(sim) => Some(simulator(&home, guest, sim, &stage.join("Slopty.app"))?),
            None => None,
        };
        // The app's tests run in two groups, those that code video last and apart
        // (`crate::e2e::APP`); the simulator's in one.
        let groups: Vec<Group> = match &simulator {
            Some(udid) => vec![(
                crate::e2e::ios_filterset(run.filter).unwrap_or_default(),
                crate::e2e::ios_env(data.as_str(), debug.as_str(), accept, udid),
                false,
            )],
            None => crate::e2e::app_filtersets(run.filter)
                .into_iter()
                .map(|(filter, codes)| {
                    let mut env = crate::e2e::app_env(data.as_str(), debug.as_str(), accept);
                    env.retain(|(key, _)| !codes || key != crate::e2e::NO_VIDEOTOOLBOX);
                    (filter, env, codes)
                })
                .collect(),
        };
        // One app in one simulator: its tests take turns.
        let output = match if simulator.is_some() { 1 } else { threads() } {
            1 => "--no-capture".to_owned(),
            // Passing tests' output too: their MEASURE lines are what a run reports.
            n => format!("--test-threads {n} --failure-output final --success-output final"),
        };
        if most > 1 {
            println!(
                "  run {at} of {}{most}",
                if matches!(run.repeat, Repeat::UntilFail(_)) { "up to " } else { "" }
            );
        }
        let this_log = if most == 1 { log.clone() } else { kept.join(format!("run-{at}.log")) };
        ran = at;
        // The run's word: the first group that failed, else one that ran tests, else the first.
        let (mut this, mut failing, mut error) = (None::<std::process::ExitStatus>, false, None);
        for (group, (filter, env, codes)) in groups.iter().enumerate() {
            // The guest's encoder, a virtual Mac's, codes one keyframe in time or the tests that
            // code are skipped, as on a runner (`crate::e2e::coding_step`).
            if *codes {
                let probe =
                    format!("perl -e 'alarm 20; exec @ARGV' '{debug}/{}'", crate::e2e::PROBE);
                if ssh(&home, &ip, &probe).is_err() {
                    println!(
                        "  ⚠ {guest}'s encoder does not answer: the tests that code video are skipped"
                    );
                    continue;
                }
            }
            let mut command = tart(&home);
            command.args(["exec", guest, "/usr/bin/env"]);
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
                     --run-ignored only {output} --retries 0 --no-fail-fast -E \"$0\""
                ),
                filter,
            ]);
            // A later group's log follows the first's in the run's log.
            let group_log = if group == 0 {
                this_log.clone()
            } else {
                this_log.with_extension(format!("{group}.log"))
            };
            let ran_group = teed(command, &group_log, &home, guest, &ip);
            if group > 0 {
                append(&this_log, &group_log)?;
            }
            let ran_group = match ran_group {
                Ok(ran_group) => ran_group,
                Err(failed) => {
                    error = Some(failed);
                    break;
                }
            };
            // A group its filter leaves empty passes: the caller's filter, or no test that codes.
            let empty = ran_group.code() == Some(crate::e2e::NO_TESTS_RUN);
            let group_passed = ran_group.code() == Some(0) || empty;
            if failing {
                continue;
            }
            if !group_passed {
                failing = true;
                this = Some(ran_group);
            } else if this.is_none_or(|kept| kept.code() != Some(0)) {
                this = Some(ran_group);
            }
        }
        if let Some(error) = error {
            status = Some(Err(error));
            break;
        }
        let Some(this) = this else {
            status = Some(Err(anyhow::anyhow!("no group of the {case} case ran")));
            break;
        };
        let passed = this.code() == Some(0)
            || (run.filter.is_some() && this.code() == Some(crate::e2e::NO_TESTS_RUN));
        if !passed && most > 1 {
            failed = failed.saturating_add(1);
            let _first = first_failed.get_or_insert(at);
            let dir = kept.join(format!("run-{at}"));
            keep_failed(&home, &ip, &data, &dir);
            println!("  run {at} FAILED: its log, data, frames and stack roots kept in {dir}");
        }
        status = Some(Ok(this));
        if !passed && (most == 1 || matches!(run.repeat, Repeat::UntilFail(_))) {
            break;
        }
    }
    if most > 1 {
        // `-E failed` reads the first failed run's log, or the last run's when none failed.
        let chosen = kept.join(format!("run-{}.log", first_failed.unwrap_or(ran)));
        let _copied = std::fs::copy(&chosen, &log);
    }
    if let Some(sim) = run.ios {
        // The run's simulator goes, and the next run's is made ready while the guest idles.
        let _dropped = drop_devices(&home, guest, &run_device(sim));
        spare(&home, &slot, sim);
    }
    let status = status.context("no run")??;
    let tests = copying.elapsed();
    stop_when_idle(&home, &slot);

    // Renders and diffs of changed frames, as a runner's come back: the check's own, since
    // another lane's run in the other guest brings back its own at the same time.
    let renders = root.join("target/remote").join(run.name).join("artifacts");
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
    let passed = failed == 0
        && (code == Some(0) || (run.filter.is_some() && code == Some(crate::e2e::NO_TESTS_RUN)));
    if most > 1 {
        println!("  {}", repeat_verdict(run.repeat, failed, ran));
    }
    if !passed {
        let failed = super::failed_tests(&text);
        for test in &failed {
            println!("        FAIL {test}");
        }
        println!("  every line: {log}; renders and diffs: {renders}");
    }
    Ok(Outcome { passed, build: built, tests, summary, guest })
}

/// Keep a failed run's whole data directory (its log, dumps and frames) and every stack root it
/// left in `dir` here, before the next run clears them. Every root, marked for inspection or
/// not: the simulator case's shared root is never marked, and the run before cleared them all.
fn keep_failed(home: &Path, ip: &str, data: &Path, dir: &Path) {
    let gathered = ssh(
        home,
        ip,
        &format!(
            "mkdir -p '{data}/roots' && find \"$(getconf DARWIN_USER_TEMP_DIR)\" /private/tmp -maxdepth 1 \
             -type d -name 'slopty-e2e-*' ! -name 'slopty-e2e-bin-*' -exec sh -c \
             'mv \"$@\" \"$0\"' '{data}/roots' {{}} +"
        ),
    );
    let from = format!("{ip}:{data}/");
    let pulled = std::fs::create_dir_all(dir)
        .map_err(anyhow::Error::from)
        .and_then(|()| rsync(home, &["--no-specials", "--no-devices", &from, dir.as_str()], dir));
    if let Err(error) = gathered.and(pulled) {
        println!("  keeping the failed run's artifacts: {error:#}");
    }
}

/// `simctl` in `guest`, through `tart exec`: in the session the tests run in, whose simulator
/// service is the one their `simctl launch` reaches.
fn simctl(home: &Path, guest: &str, args: &[&str]) -> Result<String> {
    let mut all = vec!["exec", guest, "xcrun", "simctl"];
    all.extend_from_slice(args);
    tart_out(home, &all)
}

/// `guest`'s pristine simulator for `sim`, shut down: never booted by a run, only cloned; its
/// UDID. Made, when there is none, as the device the runtimes there take
/// (`ios::choose_simulator`, as a runner makes its own), then booted once and left up until it
/// has settled ([`settle`]), so a clone carries none of that work. A simulator past `bootstatus`
/// is not done: its daemons held all four of the guest's cores for 18–20 minutes after a first
/// boot, and a clone of one shut down at `bootstatus` did all of it again. A clone of a settled
/// one was done in 3.5 minutes. One not marked [`SETTLED`] (its settle cut short) is settled
/// again before it is used.
fn pristine(home: &Path, guest: &str, sim: crate::ios::SimKind) -> Result<String> {
    let name = sim.name();
    let dir = |udid: &str| format!("/Users/{USER}/Library/Developer/CoreSimulator/Devices/{udid}");
    let devices = simctl(home, guest, &["list", "devices", "available", "-j"])?;
    let udid = if let Some(udid) = named_device(&devices, name)? {
        if tart_out(home, &["exec", guest, "ls", &dir(&udid)])?.lines().any(|l| l == SETTLED) {
            return Ok(udid);
        }
        println!("  settling simulator {name:?} in {guest}: booted before, never settled");
        udid
    } else {
        let runtimes = simctl(home, guest, &["list", "runtimes", "-j"])?;
        let devicetypes = simctl(home, guest, &["list", "devicetypes", "-j"])?;
        let chosen = crate::ios::choose_simulator(sim, &runtimes, &devicetypes)?
            .with_context(|| format!("no iOS runtime in {guest} takes an {}", sim.family()))?;
        println!("  creating simulator {name:?} in {guest}: {}", chosen.described);
        simctl(home, guest, &["create", name, &chosen.device_type, &chosen.runtime])?
    };
    let started = Instant::now();
    simctl(home, guest, &["bootstatus", &udid, "-b"])?;
    let booted = started.elapsed();
    let settled = settle(home, guest, PRISTINE_SETTLE)?;
    simctl(home, guest, &["shutdown", &udid])?;
    match settled {
        Some(after) => {
            tart_out(home, &["exec", guest, "touch", &format!("{}/{SETTLED}", dir(&udid))])?;
            println!("  booted {name} in {guest} in {booted:.1?}, settled {after:.0?} after");
        }
        None => println!(
            "  booted {name} in {guest} in {booted:.1?}, still busy {PRISTINE_SETTLE:?} after: \
             settled again on its next use"
        ),
    }
    Ok(udid)
}

/// The file in a pristine simulator's directory that says [`pristine`] let it settle.
const SETTLED: &str = "slopty-settled";

/// The share of the guest's CPU left idle, in percent, over which a simulator booted in it has
/// settled: 0–35 while its daemons run (`SetStoreUpdateService`, `MTLCompilerService`,
/// `photolibraryd`, `triald` and `WindowServer`, each up to a core), 60–99 after. A test started
/// before waited past its 30 s for the worker's address and the agent's start.
const SETTLED_IDLE: f64 = 50.0;

/// The longest [`pristine`] leaves a simulator up to settle: a first boot's work took 18–20
/// minutes.
const PRISTINE_SETTLE: Duration = Duration::from_mins(40);

/// The longest a run waits for its simulator to settle: a clone of a settled one took 132–138 s
/// after the app's install, and up to 291 s with this Mac's lanes building; a spare booted as
/// the run before ended took 19–31 s.
const RUN_SETTLE: Duration = Duration::from_mins(10);

/// Wait, for at most `most`, until `guest`'s CPU is at least [`SETTLED_IDLE`] idle on two
/// samples in a row; how long it took, or `None` if it was still busy.
fn settle(home: &Path, guest: &str, most: Duration) -> Result<Option<Duration>> {
    let started = Instant::now();
    loop {
        // `top` itself waits between its samples: 8 s a round.
        let top = tart_out(home, &["exec", guest, "top", "-l", "3", "-s", "4", "-n", "0"])?;
        if idle_after_first(&top)?.iter().all(|&idle| idle >= SETTLED_IDLE) {
            return Ok(Some(started.elapsed()));
        }
        if started.elapsed() >= most {
            return Ok(None);
        }
    }
}

/// The CPU's idle share, in percent, of each sample `top -l <n> -n 0` printed but the first,
/// which counts from the boot rather than from the sample before.
fn idle_after_first(top: &str) -> Result<Vec<f64>> {
    let mut idle = Vec::new();
    for usage in top.lines().filter_map(|l| l.trim().strip_prefix("CPU usage:")) {
        let share = usage
            .split(',')
            .find_map(|part| part.trim().strip_suffix("% idle"))
            .with_context(|| format!("no idle share in `CPU usage:{usage}`"))?;
        idle.push(share.parse::<f64>().with_context(|| format!("the idle share {share:?}"))?);
    }
    ensure!(idle.len() >= 2, "`top` printed {} CPU samples, not two or more:\n{top}", idle.len());
    idle.remove(0);
    Ok(idle)
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
/// boot took 34–96 s idle and 445 s with this Mac's lanes building, and its settle 3.5 minutes
/// more, all of it on a run's way before; the next run takes the spare as it is, booted, booting
/// or settling, and clones its own only when there is none. Nothing runs in a spare before a run
/// takes it, so it is as fresh as one cloned for the run.
fn spare(home: &Path, slot: &crate::vm::Slot, sim: crate::ios::SimKind) {
    use std::os::unix::process::CommandExt as _;
    let guest = slot.guest();
    let name = spare_device(sim);
    let ready = || -> Result<Option<String>> {
        let devices = simctl(home, guest, &["list", "devices", "-j"])?;
        if named_device(&devices, &name)?.is_some() {
            return Ok(None);
        }
        Ok(Some(pristine(home, guest, sim)?))
    };
    let Ok(Some(source)) = ready() else { return };
    let script = format!(
        "u=$(xcrun simctl clone {source} '{name}') && xcrun simctl boot \"$u\"; \
         xcrun simctl bootstatus \"$u\" -b >/dev/null"
    );
    // Under the guest's lock, taken once this run lets go of it (or given up after a minute): a
    // spare booting beside another run's tests took its cores, and one booting into an app run
    // its CoreAudio. A run that comes meanwhile waits for it, then takes it or drops it. `-k`,
    // as for the idle watcher ([`stop_when_idle`]).
    let lock = crate::vm::slot_lock(home, guest);
    let tart_home = home.join("tart");
    let held = format!(
        "printf '{SPARE_HOLDER}' > '{lock}'; exec /usr/bin/env TART_HOME='{tart_home}' \
         tart exec {guest} /bin/sh -c \"$0\""
    );
    let _spare = Command::new("/usr/bin/lockf")
        .args(["-k", "-t", "60", lock.as_str(), "/bin/sh", "-c", &held, &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

/// A simulator for one run in `guest`, booted with `app` installed; its UDID. A clone of the
/// pristine one, so every run starts as a runner's freshly made simulator does: no app data,
/// notifications not yet asked, the keyboard, appearance and pasteboard as made (a simulator
/// the runs kept kept the app's notification grant, and every iOS frame showed it). A clone of a
/// migrated simulator boots in a reboot's time, and the tests wait until it has settled.
fn simulator(home: &Path, guest: &str, sim: crate::ios::SimKind, app: &Path) -> Result<String> {
    let started = Instant::now();
    let name = run_device(sim);
    let devices = simctl(home, guest, &["list", "devices", "-j"])?;
    let spare = named_device(&devices, &spare_device(sim))?;
    // Only the run's simulator is up: two beside the tests left the guest 64 MB free, in swap,
    // and the app hung 15 s at a time.
    quiet(home, guest, spare.as_deref())?;
    let (udid, how) = if let Some(udid) = spare {
        simctl(home, guest, &["rename", &udid, &name])?;
        (udid, "took the spare")
    } else {
        let source = pristine(home, guest, sim)?;
        let udid = simctl(home, guest, &["clone", &source, &name])?;
        let _booting = simctl(home, guest, &["boot", &udid]);
        (udid, "booted a fresh")
    };
    // Boots it if it is not yet (a spare its process has not booted yet), else waits for it.
    simctl(home, guest, &["bootstatus", &udid, "-b"])?;
    // As a phone with no keyboard: GameController would count the simulator's virtual one.
    simctl(home, guest, &crate::ios::no_hardware_keyboard(&udid))?;
    simctl(home, guest, &["install", &udid, app.as_str()])?;
    let installed = started.elapsed();
    let sim = sim.name();
    // The tests start once its daemons are done, not at every core beside them.
    match settle(home, guest, RUN_SETTLE)? {
        Some(after) => println!(
            "  {how} {sim} in {guest} and installed the app ({installed:.1?}), settled {after:.0?} after"
        ),
        None => println!(
            "  {how} {sim} in {guest} and installed the app ({installed:.1?}), still busy \
             {RUN_SETTLE:?} after: a test may wait past its time"
        ),
    }
    Ok(udid)
}

/// Every simulator in `guest` down but `keep`: runs' and spares' deleted, any other shut down.
fn quiet(home: &Path, guest: &str, keep: Option<&str>) -> Result<()> {
    let listed = simctl(home, guest, &["list", "devices", "-j"])?;
    let pristine = [crate::ios::SimKind::Iphone.name(), crate::ios::SimKind::Ipad.name()];
    for device in devices(&listed)? {
        if Some(device.udid.as_str()) == keep {
            continue;
        }
        if pristine.contains(&device.name.as_str()) {
            if device.state != "Shutdown" {
                simctl(home, guest, &["shutdown", &device.udid])?;
            }
        } else if device.name.ends_with(" run") || device.name.ends_with(" spare") {
            let _shut = simctl(home, guest, &["shutdown", &device.udid]);
            simctl(home, guest, &["delete", &device.udid])?;
        } else if device.state != "Shutdown" {
            simctl(home, guest, &["shutdown", &device.udid])?;
        }
    }
    Ok(())
}

/// Shut down and delete every simulator named `name` in `guest` (a run's, a stopped run's, a
/// spare).
fn drop_devices(home: &Path, guest: &str, name: &str) -> Result<()> {
    let devices = simctl(home, guest, &["list", "devices", "-j"])?;
    for udid in named_devices(&devices, name)? {
        let _shut = simctl(home, guest, &["shutdown", &udid]);
        simctl(home, guest, &["delete", &udid])?;
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

/// One nextest run of a case: its filterset, its environment, and whether its tests code video.
type Group = (String, Vec<(String, String)>, bool);

/// `from`'s lines after `to`'s.
fn append(to: &Path, from: &Path) -> Result<()> {
    use std::io::Write as _;
    let lines = std::fs::read(from).with_context(|| format!("read {from}"))?;
    let mut file =
        std::fs::OpenOptions::new().append(true).open(to).with_context(|| format!("open {to}"))?;
    file.write_all(&lines).with_context(|| format!("append to {to}"))
}

/// How often a run reads the guest's boot ([`watch_boot`]).
const BOOT_WATCH: Duration = Duration::from_secs(20);

/// How long the guest may go unanswered mid-run before the run is given up: a panicking guest
/// writes its core and boots again in about a minute.
const UNANSWERED: Duration = Duration::from_mins(4);

/// Run `command`, a `tart exec` into the guest at `ip`, its output into `log` and, less the lines
/// of what passed, onto this terminal; how it ended. A guest that boots again under it, as one
/// whose kernel panicked does, leaves `tart exec` waiting for an answer that never comes (a run
/// sat 69 minutes so on 2026-10-30, holding the guest from every other lane), so
/// [`watch_boot`] ends it and the run fails, saying why.
fn teed(
    mut command: Command,
    log: &Path,
    home: &Path,
    guest: &'static str,
    ip: &str,
) -> Result<std::process::ExitStatus> {
    use std::io::{BufRead as _, Write as _};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    let mut file = File::create(log).with_context(|| format!("create {log}"))?;
    let booted = boot_session(home, ip).context("read the guest's boot before its run")?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("tart exec")?;
    let (done, ended) = (Arc::new(AtomicBool::new(false)), Arc::new(OnceLock::<String>::new()));
    let watch = {
        let (home, ip, pid) = (home.to_owned(), ip.to_owned(), child.id());
        let (done, ended) = (Arc::clone(&done), Arc::clone(&ended));
        std::thread::spawn(move || {
            if let Some(why) = watch_boot(&home, guest, &ip, &booted, &done) {
                let _set = ended.set(why);
                // Its pipes close with it, and the lines below end.
                let _killed = Command::new("/bin/kill").arg(pid.to_string()).status();
            }
        })
    };
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
    let status = child.wait()?;
    done.store(true, Ordering::Relaxed);
    ensure!(watch.join().is_ok(), "the guest's watch panicked");
    if let Some(why) = ended.get() {
        bail!("{why}");
    }
    Ok(status)
}

/// The guest's boot, which every boot names anew: its kernel's boot session.
fn boot_session(home: &Path, ip: &str) -> Result<String> {
    let session = ssh_out(home, ip, "sysctl -n kern.bootsessionuuid")?.trim().to_owned();
    ensure!(!session.is_empty(), "the guest names no boot session");
    Ok(session)
}

/// Read the guest's boot every [`BOOT_WATCH`] until `done`: why the run is over when the guest
/// booted again since `booted`, or has not answered for [`UNANSWERED`].
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling a guest")]
fn watch_boot(
    home: &Path,
    guest: &str,
    ip: &str,
    booted: &str,
    done: &std::sync::atomic::AtomicBool,
) -> Option<String> {
    use std::sync::atomic::Ordering;
    let mut answered = Instant::now();
    loop {
        let waited = Instant::now();
        while waited.elapsed() < BOOT_WATCH {
            if done.load(Ordering::Relaxed) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        match boot_session(home, ip) {
            Ok(now) if now == booted => answered = Instant::now(),
            Ok(_) => {
                return Some(format!("{guest} booted again mid-run; {}", panic_report(home, ip)));
            }
            Err(_) if answered.elapsed() >= UNANSWERED => {
                return Some(format!(
                    "{guest} has not answered for {:.0?}; {}",
                    answered.elapsed(),
                    panic_report(home, ip)
                ));
            }
            Err(_) => {}
        }
    }
}

/// What the guest's newest kernel panic of the last half hour says, and where its report is.
fn panic_report(home: &Path, ip: &str) -> String {
    const REPORTS: &str = "/Library/Logs/DiagnosticReports";
    let script = format!(
        "f=$(find {REPORTS} -maxdepth 1 -name '*.panic' -mmin -30 2>/dev/null | sort | tail -1); \
         [ -n \"$f\" ] || exit 0; echo \"$f\"; \
         grep -o 'panic(cpu [^@]*@[A-Za-z0-9_.]*:[0-9]*' \"$f\" | head -1 | tr -d '\\\\'; \
         grep -o 'Panicked task [^\\\\]*' \"$f\" | head -1 | sed 's/.*: //'"
    );
    let said = ssh_out(home, ip, &script).unwrap_or_default();
    let mut lines = said.lines().map(str::trim).filter(|l| !l.is_empty());
    match (lines.next(), lines.next(), lines.next()) {
        (Some(report), Some(panic), task) => {
            let task = task.map_or_else(String::new, |task| format!(", in {task}"));
            format!("its kernel panicked: {panic}{task} ({report})")
        }
        _ => format!("a kernel panic leaves its report in the guest's {REPORTS}"),
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    /// The base is made from the live lane's: Xcode, the runners' iOS runtime, both simulators.
    /// Live: 28 minutes and an 8.5 GB download, past nextest's deadline for xtask (`cargo test -p
    /// xtask --lib -- --ignored a_base_is_made`); it makes `slopty-27-xcode-test` and deletes it.
    #[test]
    #[ignore = "live: makes a macOS guest"]
    fn a_base_is_made_from_the_live_lane_s() {
        let home = super::home();
        let base = "slopty-27-xcode-test";
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

    /// A run tries the guest its lane used last first, then a running one, then the rest, in
    /// the pool's order; a name the pool no longer has, or a guest of another name that runs,
    /// changes nothing.
    #[test]
    fn a_run_tries_its_lane_s_guest_then_a_running_one() {
        let [first, second] = crate::vm::E2E_GUESTS;
        let list = |running: &[&str]| {
            let rows = [first, second, "slopty-27-e2e-20", "slopty-27-xcode"].map(|guest| {
                let state = if running.contains(&guest) { "running" } else { "stopped" };
                format!("local {guest} 90 GB 71 GB 1 hour ago {state}")
            });
            format!("Source Name Disk Size Accessed State\n{}\n", rows.join("\n"))
        };
        assert_eq!(super::preferred(None, &list(&[])), [first, second]);
        assert_eq!(super::preferred(None, &list(&[second])), [second, first]);
        assert_eq!(super::preferred(Some(first), &list(&[second])), [first, second]);
        assert_eq!(super::preferred(Some(second), &list(&[])), [second, first]);
        let others = super::preferred(Some("slopty-26-e2e"), &list(&["slopty-27-e2e-20"]));
        assert_eq!(others, [first, second]);
    }

    /// The guest's simulator is found by name across runtimes, never by another's name; one is
    /// made as `ios::choose_simulator` chooses (its own test).
    #[test]
    fn the_simulator_is_found_by_name() {
        let devices = r#"{"devices": {
            "com.apple.CoreSimulator.SimRuntime.iOS-27-0": [
                {"name": "Slopty iPad", "udid": "76FFF7A1-9F82-4F1D-8E6C-2FEDA6CCF705"},
                {"name": "Slopty iPhone", "udid": "EAA99FB6-E2BE-4FFE-B921-2413AC6653E4"}
            ]}}"#;
        assert_eq!(
            super::named_device(devices, "Slopty iPhone").expect("parses").as_deref(),
            Some("EAA99FB6-E2BE-4FFE-B921-2413AC6653E4")
        );
        assert_eq!(super::named_device(devices, "Slopty Watch").expect("parses"), None);
    }

    /// A guest's idleness is each of `top`'s samples but the first (which counts from the boot),
    /// as the guest's `top` prints them; output with fewer than two samples, or a sample without
    /// its idle share, is refused rather than read as settled or busy.
    #[test]
    fn the_guest_s_idleness_is_top_s_samples_after_the_first() {
        let sample = |idle: &str| {
            format!(
                "Processes: 567 total, 2 running, 565 sleeping, 1713 threads \n\
                 2026/10/30 09:12:41\n\
                 Load Avg: 1.91, 3.02, 9.47 \n\
                 CPU usage: 4.87% user, 21.95% sys, {idle}% idle \n\
                 SharedLibs: 501M resident, 81M data, 39M linkedit.\n"
            )
        };
        let top = [sample("73.17"), sample("0.0"), sample("99.54")].concat();
        assert_eq!(super::idle_after_first(&top).expect("parses"), [0.0, 99.54]);
        let settled = [sample("0.0"), sample("93.93"), sample("61.33")].concat();
        assert!(super::idle_after_first(&settled).expect("parses").iter().all(|&i| i >= 50.0));
        let _one_sample = super::idle_after_first(&sample("99.0")).unwrap_err();
        let _no_sample = super::idle_after_first("top: failed to read the kernel\n").unwrap_err();
        let unshared = [sample("1.0"), "CPU usage: 4.87% user, 21.95% sys \n".to_owned()].concat();
        let _no_idle_share = super::idle_after_first(&unshared).unwrap_err();
        let _not_a_share =
            super::idle_after_first(&[sample("1.0"), sample("n/a")].concat()).unwrap_err();
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

    /// A repeated run's verdict: how often it failed, and the clean runs in a row after which a
    /// fix holds at 95 % for the rate it showed: 14 for a flake failing 1 run in 5.
    #[test]
    fn a_repeated_run_says_how_often_it_failed_and_what_a_fix_needs() {
        use super::Repeat::{Times, UntilFail};
        assert_eq!(super::clean_runs_for(0.2), 14);
        assert_eq!(super::clean_runs_for(0.5), 5);
        assert_eq!(super::clean_runs_for(0.1), 29);
        assert_eq!(super::clean_runs_for(0.01), 299);
        let said = super::repeat_verdict(Times(10), 2, 10);
        assert!(said.starts_with("2 of 10 failed, 1 in 5.0") && said.contains("--until-fail 14"));
        let said = super::repeat_verdict(Times(14), 0, 14);
        assert!(said.starts_with("0 of 14 failed") && said.contains("1 run in 5.2"), "{said}");
        let said = super::repeat_verdict(Times(4), 4, 4);
        assert!(said.contains("fails every run"), "{said}");
        let said = super::repeat_verdict(UntilFail(30), 1, 10);
        assert!(
            said.starts_with("failed at run 10 of up to 30") && said.contains("--until-fail 29")
        );
        let said = super::repeat_verdict(UntilFail(30), 1, 1);
        assert!(said.contains("`--repeat <N>`"), "{said}");
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

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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path as Path, Utf8PathBuf as PathBuf};
use xshell::Shell;

use super::Goldens;
use crate::ios::SimKind;

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
    /// Which of `slopty-e2e`'s targets runs.
    pub target: Target,
    /// How many times the case runs in this one session.
    pub repeat: Repeat,
}

/// The e2e target a run takes in the guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The app's (`app`).
    App,
    /// The simulator's (`ios`), on this device.
    Ios(SimKind),
    /// The server's (`server`): the CLI and MCP through a real server and worker.
    Server,
}

impl Target {
    /// Its test target's name.
    pub const fn case(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Ios(_) => "ios",
            Self::Server => "server",
        }
    }

    /// The simulator it runs in, if any.
    pub const fn sim(self) -> Option<SimKind> {
        match self {
            Self::Ios(sim) => Some(sim),
            Self::App | Self::Server => None,
        }
    }
}

/// How many times a run's case runs, in one guest session: booted and copied once, each time
/// on a simulator reset as a run's is ([`prepare_simulator`]), its failures' artifacts kept apart
/// ([`e2e`]).
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
    /// Each phase's time: the build's, then the guest's, which ran beside it ([`ready_guest`]),
    /// then what the run did once both were done.
    pub phases: Phases,
    /// The whole run, from the build to the renders back.
    pub wall: Duration,
    /// What nextest ran, over every group of the case (the app runs two), from their `Summary`
    /// lines: the last one alone counted the one test that codes video, and a whole suite read as
    /// one test.
    pub count: Count,
}

/// Each phase of a run and its time, in the order the run took them; a phase taken again (a
/// repeated run's tests) adds to its time.
#[derive(Debug, Default)]
pub struct Phases(Vec<(&'static str, Duration)>);

impl Phases {
    /// `took` more of `phase`.
    pub fn add(&mut self, phase: &'static str, took: Duration) {
        match self.0.iter_mut().find(|(name, _)| *name == phase) {
            Some((_, time)) => *time = time.saturating_add(took),
            None => self.0.push((phase, took)),
        }
    }

    /// Each phase and its time.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, Duration)> + '_ {
        self.0.iter().copied()
    }

    /// `other`'s phases after these.
    fn extend(&mut self, other: Self) {
        for (phase, took) in other.0 {
            self.add(phase, took);
        }
    }
}

impl std::fmt::Display for Phases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let said: Vec<String> = self
            .0
            .iter()
            .map(|(name, took)| format!("{name} {:.1}s", took.as_secs_f64()))
            .collect();
        f.write_str(&said.join(", "))
    }
}

/// What nextest ran in a run's groups, from their `Summary` lines.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Count {
    /// Tests run, over every group.
    pub run: u32,
    /// Each outcome nextest named (`passed`, `failed`, `timed out`, …) and how many, over every
    /// group, `skipped` aside.
    pub outcomes: Vec<(String, u32)>,
    /// The tests in the target that no group ran: its filters left them out.
    pub skipped: u32,
    /// How many `Summary` lines it read: one per group that ran.
    pub groups: u32,
}

impl Count {
    /// The count of every `Summary [ … ] N tests run: …` line in `log`. A group's run and
    /// skipped tests together are the target's, so the whole run's skipped are the target's less
    /// every group's run.
    pub fn of(log: &str) -> Self {
        let mut count = Self::default();
        let mut target = 0_u32;
        for line in log.lines() {
            let Some((_, after)) = line.split_once("Summary [") else { continue };
            let Some((_, said)) = after.split_once(']') else { continue };
            let Some((run, outcomes)) = said.split_once(" run:") else { continue };
            let Some(run) = run.split_whitespace().next().and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            count.groups = count.groups.saturating_add(1);
            count.run = count.run.saturating_add(run);
            let mut skipped = 0_u32;
            for part in outcomes.split(',') {
                // `45 passed (3 slow)`: the count, then the outcome, then any note.
                let part = part.split_once(" (").map_or(part, |(said, _note)| said).trim();
                let Some((n, outcome)) = part.split_once(' ') else { continue };
                let Ok(n) = n.parse::<u32>() else { continue };
                if outcome == "skipped" {
                    skipped = n;
                } else if let Some((_, total)) =
                    count.outcomes.iter_mut().find(|(name, _)| name == outcome)
                {
                    *total = total.saturating_add(n);
                } else {
                    count.outcomes.push((outcome.to_owned(), n));
                }
            }
            target = target.max(run.saturating_add(skipped));
        }
        count.skipped = target.saturating_sub(count.run);
        count
    }
}

impl std::fmt::Display for Count {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.groups == 0 {
            return f.write_str("no summary: see the log");
        }
        let tests = if self.run == 1 { "test" } else { "tests" };
        write!(f, "{} {tests} run", self.run)?;
        let outcomes: Vec<String> = self.outcomes.iter().map(|(o, n)| format!("{n} {o}")).collect();
        if !outcomes.is_empty() {
            write!(f, ": {}", outcomes.join(", "))?;
        }
        write!(f, "; {} skipped", self.skipped)?;
        if self.groups > 1 {
            write!(f, " ({} groups)", self.groups)?;
        }
        Ok(())
    }
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
    let bins = crate::e2e::bin_dir(&dir.join("e2e-target"));
    let lanes = dir.parent().context("a lane's directory sits in target/local")?;
    let lane = dir.file_name().context("a lane's directory has a name")?;
    let _unavailable = simctl(home, guest, &["delete", "unavailable"]);
    let listing = ssh_out(
        home,
        ip,
        &format!(
            "pkill -9 -f '{bins}/[s]lopty|slopty-e2e-bin-[0-9a-f]+/[s]lopty' ; \
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
    for sim in [SimKind::Iphone, SimKind::Ipad] {
        pristine(home, making, sim)?;
    }
    Ok(())
}

/// How often a run that waits for an e2e guest looks again.
const POOL_WAIT: Duration = Duration::from_secs(2);

/// An e2e guest's slot, held for one run of lane `lane` (whose directory is `dir`) that takes the
/// simulator for `sim`, or none (the app's case): the first free one in [`preferred`]'s order;
/// with both taken, whichever frees first. Given up once `stop` is set (the build failed).
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; waiting a turn")]
fn hold(
    home: &Path,
    lane: &str,
    dir: &Path,
    sim: Option<SimKind>,
    stop: &AtomicBool,
) -> Result<crate::vm::Slot> {
    std::fs::create_dir_all(home.join("locks"))?;
    let last = dir.join("e2e-guest");
    let order = preferred(
        std::fs::read_to_string(&last).ok().as_deref().map(str::trim),
        &tart_out(home, &["list"])?,
        |guest| std::fs::read_to_string(booted_note(home, guest)).ok(),
        sim.map(SimKind::name),
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
                .map(|guest| format!("{guest} by {}'s run", crate::vm::slot_holder(home, guest)))
                .into();
            println!("  waiting for an e2e guest: {}", held.join(", "));
            told = true;
        }
        stopped(stop)?;
        std::thread::sleep(POOL_WAIT);
    }
}

/// The order a run tries the e2e guests in. First those whose simulator up is the one the run
/// takes, `want` (`None` for the app's case, which takes none), then those with none up, then
/// those with another up: that one's shutdown costs the next run of its device a boot and a
/// settle ([`prepare_simulator`]). Within each, the one named `last` (the lane's last: its copies
/// of the lane's binaries are there), then those `tart list`'s `listed` says run, then the rest,
/// in the pool's order. `booted` reads what a guest's runs left up ([`note_booted`]), taken only
/// for a guest that runs: a stopped one has none up.
fn preferred(
    last: Option<&str>,
    listed: &str,
    booted: impl Fn(&str) -> Option<String>,
    want: Option<&str>,
) -> Vec<&'static str> {
    let running = |guest: &str| {
        listed.lines().any(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            words.get(1) == Some(&guest) && words.last() == Some(&"running")
        })
    };
    let mut order = crate::vm::E2E_GUESTS.to_vec();
    order.sort_by_cached_key(|&guest| {
        let up = running(guest);
        let left = if up { booted(guest) } else { None };
        let left = left.as_deref().map(str::trim);
        let simulator = if left == want {
            0_u8
        } else if left.is_none() {
            1
        } else {
            2
        };
        (simulator, Some(guest) != last, !up)
    });
    order
}

/// The file that names the simulator a guest's runs left up ([`note_booted`]).
fn booted_note(home: &Path, guest: &str) -> PathBuf {
    home.join("locks").join(format!("{guest}.simulator"))
}

/// Note, under `guest`'s lock, which simulator its runs leave up (`name`), or that none is
/// (`None`), for the next run's choice of guest ([`preferred`]).
fn note_booted(home: &Path, guest: &str, name: Option<&str>) {
    let note = booted_note(home, guest);
    let _noted = match name {
        Some(name) => std::fs::write(&note, name),
        None => std::fs::remove_file(&note),
    };
}

/// An error once `stop` is set: the build failed, and the guest's preparation for it goes no
/// further.
fn stopped(stop: &AtomicBool) -> Result<()> {
    ensure!(!stop.load(Ordering::Relaxed), "the build failed: the guest's preparation stopped");
    Ok(())
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

/// Where a run's files are, here and at the same paths in the guest.
struct Places {
    /// The lane's directory.
    dir: PathBuf,
    /// The check's tree in it.
    tree: PathBuf,
    /// The e2e build's target dir, apart from the check's own, whose test-profile builds of the
    /// same binaries would replace these.
    target: PathBuf,
    /// The binaries in it ([`crate::e2e::bin_dir`]).
    debug: PathBuf,
    /// What goes to the guest beside them: the archive, nextest and its config, the app.
    stage: PathBuf,
}

/// What this Mac builds for a run ([`build`]).
struct Built {
    /// Each step's time.
    phases: Phases,
    /// The case's test binary as a nextest archive.
    archive: PathBuf,
    /// nextest's config for the guest ([`guest_config`]).
    config: PathBuf,
    /// The simulator's app.
    bundle: Option<PathBuf>,
}

/// Build what a run of `case` takes into `places`: the binaries and the case's test binary in
/// one call, so the archive compiles nothing (`crate::e2e::build`), and for the simulator for
/// `sim` its app, with this Mac's Xcode, into the same target dir.
fn build(root: &Path, places: &Places, case: &str, sim: Option<SimKind>) -> Result<Built> {
    let sh = Shell::new()?;
    sh.change_dir(&places.tree);
    sh.set_var("LIBGHOSTTY_VT_SYS_PREBUILT_DIR", root.join("target").join(crate::prune::PREBUILT));
    crate::tools::use_zig_for(&sh, &places.tree)?;
    let target = &places.target;
    let target_arg = format!("--target-dir={target}");
    let mut phases = Phases::default();
    let building = Instant::now();
    crate::e2e::build(&sh, &[], &[case], &[&target_arg])?;
    phases.add("build", building.elapsed());
    let bundle = match sim {
        Some(sim) => {
            crate::ios::preflight(&sh, false)?;
            let _target = sh.push_env("CARGO_TARGET_DIR", target);
            let built = crate::ios::build_for_simulator(&sh, sim, "info")?;
            for (phase, took) in built.phases {
                phases.add(phase, took);
            }
            Some(built.app)
        }
        None => None,
    };
    let archive = places.stage.join(format!("{case}.tar.zst"));
    let archiving = Instant::now();
    let selection = crate::e2e::selection(&[]);
    crate::tools::step(
        &format!("archive slopty-e2e {case}"),
        &xshell::cmd!(
            sh,
            "cargo nextest archive --cargo-profile dev {target_arg} {selection...} --test {case} --archive-file {archive}"
        )
        .env(crate::gate::BINS_FRESH, "1"),
    )?;
    phases.add("archive", archiving.elapsed());
    let config = places.stage.join("nextest.toml");
    std::fs::write(&config, guest_config(&places.tree)?)?;
    Ok(Built { phases, archive, config, bundle })
}

/// A guest made ready for a run ([`ready_guest`]).
struct Ready {
    /// Its slot, held to the run's end.
    slot: crate::vm::Slot,
    /// Its address.
    ip: String,
    /// The run's simulator, booted and settled, the app not yet installed.
    udid: Option<String>,
    /// Its phases: the wait for the guest, its boot, the simulator's and its settle.
    phases: Phases,
}

/// A guest made ready for a run of lane `lane` while this Mac builds it: its slot held
/// ([`hold`]), up, the lanes it keeps pruned, the run's directories made, its copy of the
/// binaries cut to those the run takes, and the simulator for `sim` booted, reset and settled
/// ([`prepare_simulator`]), or for the app's case every simulator down. A run did each after its
/// build before, and waited for them: a stopped guest's boot 29–45 s, a fresh simulator 75–223 s
/// and its settle 19–206 s more (B's study, 2026-10-31). Given up at its next step once `stop` is
/// set (the build failed).
fn ready_guest(
    home: &Path,
    lane: &str,
    places: &Places,
    sim: Option<SimKind>,
    stop: &AtomicBool,
) -> Result<Ready> {
    let mut phases = Phases::default();
    let waiting = Instant::now();
    let slot = hold(home, lane, &places.dir, sim, stop)?;
    phases.add("wait", waiting.elapsed());
    let guest = slot.guest();
    stop_when_idle(home, &slot);
    let booting = Instant::now();
    let ip = up(home, guest)?;
    phases.add("boot", booting.elapsed());
    stopped(stop)?;
    prune(home, guest, &ip, &places.dir)?;
    let Places { tree, target, debug, stage, .. } = places;
    // The guest's copy holds the binaries the run takes and nothing else: one a build no longer
    // makes (the plain app, before the e2e build named its binaries) is gone from it.
    // And the binaries of the build before it named the Mac's triple, beside it.
    let stale = if *debug == target.join("debug") {
        String::new()
    } else {
        format!(" && rm -rf '{target}/debug'")
    };
    let kept = crate::e2e::BINS.iter().fold(String::new(), |mut kept, bin| {
        let _infallible = std::fmt::Write::write_fmt(&mut kept, format_args!(" ! -name '{bin}'"));
        kept
    });
    ssh(
        home,
        &ip,
        &format!(
            "mkdir -p '{debug}' '{tree}' '{stage}/x' && rm -rf '{tree}/target/e2e' && \
             find '{debug}' -mindepth 1 -maxdepth 1{kept} -exec rm -rf {{}} +{stale}"
        ),
    )?;
    let udid = if let Some(sim) = sim {
        Some(prepare_simulator(home, guest, sim, &mut phases, stop)?)
    } else {
        // A booted simulator holds CoreAudio, and the app's audio player then takes tens of
        // seconds to open: no simulator stays up through the app's case.
        quiet(home, guest, None)?;
        None
    };
    Ok(Ready { slot, ip, udid, phases })
}

/// Run the app's e2e case, or the simulator's, of commit `sha` in lane `lane`'s directory, in
/// the guest, made ready while this Mac builds ([`ready_guest`]).
#[expect(clippy::too_many_lines, reason = "one run, step by step")]
pub fn e2e(root: &Path, lane: &str, sha: &str, run: &Run<'_>) -> Result<Outcome> {
    let started = Instant::now();
    let case = run.target.case();
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
    crate::prune::before_build();
    let dir = crate::local::dir(root, lane);
    let tree = crate::gate::snapshot_commit(root, &dir, sha)?;
    crate::local::target_by_config(&dir)?;
    crate::local::seed(&dir, "e2e-target");
    let target = dir.join("e2e-target");
    let stage = dir.join("guest");
    std::fs::create_dir_all(&stage)?;
    let places = Places { debug: crate::e2e::bin_dir(&target), dir, tree, target, stage };
    let home = home();
    let sim = run.target.sim();
    let stop = AtomicBool::new(false);
    let (built, ready, after_build) = std::thread::scope(|scope| {
        let guest_side = scope.spawn(|| ready_guest(&home, lane, &places, sim, &stop));
        let built = build(root, &places, case, sim);
        if built.is_err() {
            stop.store(true, Ordering::Relaxed);
        }
        let waiting = Instant::now();
        let ready = guest_side
            .join()
            .unwrap_or_else(|_panicked| Err(anyhow::anyhow!("the guest's preparation panicked")));
        (built, ready, waiting.elapsed())
    });
    let Built { mut phases, archive, config, bundle } = built?;
    let Ready { slot, ip, udid, phases: guest_phases } = ready?;
    phases.extend(guest_phases);
    // What the guest's preparation took past the build: the part of it the build did not hide.
    phases.add("guest after build", after_build);
    let guest = slot.guest();
    let Places { tree, debug, stage, .. } = places;
    let copying = Instant::now();
    // Only the binaries that changed, each whole: rsync's delta transfer read and checksummed
    // both sides of every changed binary, 500 MB of them, and took 233 s with this Mac's lanes
    // building. Never `--inplace`: a file rewritten where the guest reads it was taken for a
    // corrupt archive, and a binary a killed run still maps keeps its old inode this way. Not a
    // shared directory: the guest's virtio-fs view kept a file the host had replaced by rename
    // as missing, as cargo replaces a binary and Xcode the app (2026-10-17).
    let mut args: Vec<&str> = vec!["--whole-file"];
    args.extend(crate::e2e::BINS);
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
    phases.add("copy", copying.elapsed());
    println!(
        "  {guest} up; {} binaries, the archive and sources copied in {:.1?}",
        crate::e2e::BINS.len(),
        copying.elapsed()
    );

    let data = tree.join("target/e2e");
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
    let mut prepared = udid;
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
                    "pkill -9 -f '{debug}/[s]lopty|slopty-e2e-bin-[0-9a-f]+/[s]lopty' ; rm -rf '{data}' ; find \"$(getconf DARWIN_USER_TEMP_DIR)\" /private/tmp -maxdepth 1 \
                     -name 'slopty-e2e-*' ! -name 'slopty-e2e-bin-*' -exec rm -rf {{}} +"
                ),
            )?;
        }
        // The app installed in the simulator, in the session the tests run in: the first run's
        // made ready under the build, a repeat's reset as a run alone starts.
        let simulator = match sim {
            Some(sim) => {
                let udid = match prepared.take() {
                    Some(udid) => udid,
                    None => prepare_simulator(&home, guest, sim, &mut phases, &stop)?,
                };
                install(&home, guest, &udid, &stage.join("Slopty.app"), &mut phases)?;
                Some(udid)
            }
            None => None,
        };
        // The app's tests run in two groups, those that code video last and apart
        // (`crate::e2e::APP`); the simulator's and the server's in one.
        let groups: Vec<Group> = match (&simulator, run.target) {
            (Some(udid), _) => vec![(
                crate::e2e::ios_filterset(run.filter),
                crate::e2e::ios_env(data.as_str(), debug.as_str(), accept, udid),
                false,
            )],
            (None, Target::Server) => vec![(
                crate::e2e::server_filterset(run.filter),
                crate::e2e::server_env(data.as_str(), debug.as_str(), accept),
                false,
            )],
            (None, _) => crate::e2e::app_filtersets(run.filter)
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
            let testing = Instant::now();
            let ran_group = teed(command, &group_log, &home, guest, &ip);
            phases.add(if *codes { "coding tests" } else { "tests" }, testing.elapsed());
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
    // The run's simulator stays up for the next run of its device ([`prepare_simulator`]).
    let status = status.context("no run")??;
    stop_when_idle(&home, &slot);
    let back = Instant::now();

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
    phases.add("back", back.elapsed());
    let text = std::fs::read_to_string(&log).unwrap_or_default();
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
    let count = Count::of(&text);
    Ok(Outcome { passed, guest, phases, wall: started.elapsed(), count })
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
fn pristine(home: &Path, guest: &str, sim: SimKind) -> Result<String> {
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
    let settled = settle(home, guest, PRISTINE_SETTLE, &AtomicBool::new(false))?;
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
/// after the app's install, and up to 291 s with this Mac's lanes building; one booted as the run
/// before ended took 19–31 s.
const RUN_SETTLE: Duration = Duration::from_mins(10);

/// Wait, for at most `most`, until `guest`'s CPU is at least [`SETTLED_IDLE`] idle on two
/// samples in a row; how long it took, or `None` if it was still busy. Given up once `stop` is
/// set.
fn settle(home: &Path, guest: &str, most: Duration, stop: &AtomicBool) -> Result<Option<Duration>> {
    let started = Instant::now();
    loop {
        stopped(stop)?;
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

/// The name of the simulator a guest keeps for `sim`'s runs ([`prepare_simulator`]), a clone of
/// the pristine one.
fn run_device(sim: SimKind) -> String {
    format!("{} run", sim.name())
}

/// The run's simulator for `sim` in `guest`, booted, reset and settled, the app not yet
/// installed: its UDID. Each guest keeps one simulator per device from run to run
/// ([`run_device`]), left up after its run, and the next run of that device resets what the app
/// left in it ([`reset_app`]) instead of cloning the pristine one and booting the clone, which
/// took 75–223 s and a settle of 19–206 s more. The first run of a device in a guest clones the
/// pristine one; a kept one an app run shut down boots again; one that will not boot or reset is
/// replaced by a clone. Only the run's simulator is up ([`quiet`]). `stop` as for [`settle`].
fn prepare_simulator(
    home: &Path,
    guest: &str,
    sim: SimKind,
    phases: &mut Phases,
    stop: &AtomicBool,
) -> Result<String> {
    let started = Instant::now();
    let name = run_device(sim);
    let listed = devices(&simctl(home, guest, &["list", "devices", "-j"])?)?;
    let kept = listed.into_iter().find(|device| device.name == name);
    // Only the run's simulator is up: two beside the tests left the guest 64 MB free, in swap,
    // and the app hung 15 s at a time.
    quiet(home, guest, kept.as_ref().map(|device| device.udid.as_str()))?;
    let mut how = match &kept {
        Some(device) if device.state == "Booted" => "reset the kept",
        Some(_) => "booted the kept",
        None => "booted a fresh",
    };
    let udid = match kept {
        Some(device) => match ready_kept(home, guest, &device.udid) {
            Ok(()) => device.udid,
            Err(error) => {
                println!(
                    "  the kept {name} in {guest} did not boot or reset ({error:#}): a fresh one"
                );
                drop_devices(home, guest, &name)?;
                how = "booted a fresh";
                fresh(home, guest, sim, &name)?
            }
        },
        None => fresh(home, guest, sim, &name)?,
    };
    note_booted(home, guest, Some(sim.name()));
    // As a phone with no keyboard: GameController would count the simulator's virtual one.
    simctl(home, guest, &crate::ios::no_hardware_keyboard(&udid))?;
    let ready = started.elapsed();
    phases.add("simulator", ready);
    // The tests start once its daemons are done, not at every core beside them.
    let settling = Instant::now();
    let settled = settle(home, guest, RUN_SETTLE, stop)?;
    phases.add("settle", settling.elapsed());
    let sim = sim.name();
    match settled {
        Some(after) => {
            println!("  {how} {sim} in {guest} ({ready:.1?}), settled {after:.0?} after");
        }
        None => println!(
            "  {how} {sim} in {guest} ({ready:.1?}), still busy {RUN_SETTLE:?} after: a test may \
             wait past its time"
        ),
    }
    Ok(udid)
}

/// The kept simulator `udid` booted (an app run shut it down) and reset ([`reset_app`]).
fn ready_kept(home: &Path, guest: &str, udid: &str) -> Result<()> {
    // Boots it if it is down, and returns at once if it is up.
    simctl(home, guest, &["bootstatus", udid, "-b"])?;
    reset_app(home, guest, udid)
}

/// A new simulator `name` for `sim` in `guest`, cloned from the pristine one and booted; its
/// UDID. A clone of a settled simulator boots in a reboot's time ([`pristine`]).
fn fresh(home: &Path, guest: &str, sim: SimKind, name: &str) -> Result<String> {
    let source = pristine(home, guest, sim)?;
    let udid = simctl(home, guest, &["clone", &source, name])?;
    simctl(home, guest, &["bootstatus", &udid, "-b"])?;
    Ok(udid)
}

/// Take the app and what it left out of the booted simulator `udid`, so a kept simulator starts
/// a run as a runner's new one does: the app quit and uninstalled, with its container, its
/// preferences and its notification authorisation, every privacy grant and the keychain reset.
/// An app installed over its old self kept its grant, and every iOS frame showed "The server
/// can't send notifications…" where a runner's asks; two runs in a row on one kept simulator
/// drew every frame a fresh one draws (`docs/decisions/testing.md`).
fn reset_app(home: &Path, guest: &str, udid: &str) -> Result<()> {
    let bundle = crate::ios::BUNDLE_ID;
    let installed = || simctl(home, guest, &["get_app_container", udid, bundle]).is_ok();
    if installed() {
        let _quit = simctl(home, guest, &["terminate", udid, bundle]);
        simctl(home, guest, &["uninstall", udid, bundle])?;
        ensure!(!installed(), "{bundle} is still in {udid} after its uninstall");
    }
    simctl(home, guest, &["privacy", udid, "reset", "all"])?;
    simctl(home, guest, &["keychain", udid, "reset"])?;
    Ok(())
}

/// Install `app` in the run's simulator `udid` and wait for the guest to settle after it.
fn install(home: &Path, guest: &str, udid: &str, app: &Path, phases: &mut Phases) -> Result<()> {
    let started = Instant::now();
    simctl(home, guest, &["install", udid, app.as_str()])?;
    let installed = started.elapsed();
    let settled = settle(home, guest, RUN_SETTLE, &AtomicBool::new(false))?;
    phases.add("install", started.elapsed());
    match settled {
        Some(after) => {
            println!("  installed the app in {installed:.1?}, settled {after:.0?} after");
        }
        None => println!(
            "  installed the app in {installed:.1?}, the guest still busy {RUN_SETTLE:?} after: \
             a test may wait past its time"
        ),
    }
    Ok(())
}

/// Every simulator in `guest` down but `keep`: the pristine and the kept ones ([`run_device`])
/// shut down, any other deleted. With none left up, the guest's note says so ([`note_booted`]).
fn quiet(home: &Path, guest: &str, keep: Option<&str>) -> Result<()> {
    let listed = simctl(home, guest, &["list", "devices", "-j"])?;
    let known: Vec<String> = [SimKind::Iphone, SimKind::Ipad]
        .into_iter()
        .flat_map(|sim| [sim.name().to_owned(), run_device(sim)])
        .collect();
    for device in devices(&listed)? {
        if Some(device.udid.as_str()) == keep {
            continue;
        }
        if known.contains(&device.name) {
            if device.state != "Shutdown" {
                simctl(home, guest, &["shutdown", &device.udid])?;
            }
        } else {
            let _shut = simctl(home, guest, &["shutdown", &device.udid]);
            simctl(home, guest, &["delete", &device.udid])?;
        }
    }
    if keep.is_none() {
        note_booted(home, guest, None);
    }
    Ok(())
}

/// Shut down and delete every simulator named `name` in `guest`.
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
    done: &AtomicBool,
) -> Option<String> {
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

    use crate::ios::SimKind;

    /// A run's count is every group's `Summary` added up, not the last one's: the app's two
    /// groups read as the one test that codes video, and a whole suite passed for a run of one
    /// (2026-10-31). The tests no group ran are the target's less those run.
    #[test]
    fn a_run_counts_every_group_s_summary() {
        let log = "        FAIL [   9.658s] (41/47) slopty-e2e::app transcript::a_long_session\n\
                   \u{20}    Summary [ 211.221s] 47 tests run: 45 passed (3 slow), 2 failed, 1 skipped\n\
                   error: test run failed\n\
                   \u{20}    Summary [  10.463s] 1 test run: 1 passed, 47 skipped\n";
        let count = super::Count::of(log);
        assert_eq!(count.run, 48);
        assert_eq!(count.outcomes, [("passed".to_owned(), 46), ("failed".to_owned(), 2)]);
        assert_eq!(count.skipped, 0, "every test of the target ran in one group or the other");
        assert_eq!(count.groups, 2);
        assert_eq!(count.to_string(), "48 tests run: 46 passed, 2 failed; 0 skipped (2 groups)");

        let filtered = "     Summary [  56.000s] 5 tests run: 4 passed, 1 timed out, 43 skipped\n\
                        \u{20}    Summary [   0.001s] 0 tests run: 0 passed, 48 skipped\n";
        let count = super::Count::of(filtered);
        assert_eq!((count.run, count.skipped), (5, 43));
        assert_eq!(count.outcomes, [("passed".to_owned(), 4), ("timed out".to_owned(), 1)]);
        assert_eq!(super::Count::of("no nextest here").to_string(), "no summary: see the log");
    }

    /// A phase taken again adds to its time, in the place it was first taken.
    #[test]
    fn a_phase_taken_again_adds_up() {
        let mut phases = super::Phases::default();
        phases.add("build", std::time::Duration::from_millis(13_500));
        phases.add("tests", std::time::Duration::from_secs(10));
        phases.add("simulator", std::time::Duration::from_secs(2));
        phases.add("tests", std::time::Duration::from_secs(5));
        assert_eq!(phases.to_string(), "build 13.5s, tests 15.0s, simulator 2.0s");
    }

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
        for sim in [SimKind::Iphone, SimKind::Ipad] {
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
        let none = |_guest: &str| None;
        let order = |last, listed: &str| super::preferred(last, listed, none, None);
        assert_eq!(order(None, &listing(&[])), [first, second]);
        assert_eq!(order(None, &listing(&[second])), [second, first]);
        assert_eq!(order(Some(first), &listing(&[second])), [first, second]);
        assert_eq!(order(Some(second), &listing(&[])), [second, first]);
        let others = order(Some("slopty-26-e2e"), &listing(&["slopty-27-e2e-20"]));
        assert_eq!(others, [first, second]);
    }

    /// An iOS run goes first to the guest whose kept simulator for its device is up, then to one
    /// with none up, and an app run to one with none up, before the lane's last guest; a stopped
    /// guest has none up, whatever its note says.
    #[test]
    fn a_run_goes_where_its_simulator_is_up() {
        let [first, second] = crate::vm::E2E_GUESTS;
        let iphone = SimKind::Iphone.name();
        let ipad = SimKind::Ipad.name();
        let up = |guest: &str| (guest == second).then(|| format!("{iphone}\n"));
        let both = listing(&[first, second]);
        assert_eq!(super::preferred(Some(first), &both, up, Some(iphone)), [second, first]);
        assert_eq!(super::preferred(Some(second), &both, up, None), [first, second]);
        assert_eq!(super::preferred(Some(second), &both, up, Some(ipad)), [first, second]);
        let stopped = listing(&[first]);
        assert_eq!(super::preferred(None, &stopped, up, Some(iphone)), [first, second]);
        assert_eq!(super::preferred(Some(second), &stopped, up, None), [second, first]);
    }

    /// `tart list` with the e2e guests, another and the base, those in `running` running.
    fn listing(running: &[&str]) -> String {
        let [first, second] = crate::vm::E2E_GUESTS;
        let rows = [first, second, "slopty-27-e2e-20", "slopty-27-xcode"].map(|guest| {
            let state = if running.contains(&guest) { "running" } else { "stopped" };
            format!("local {guest} 90 GB 71 GB 1 hour ago {state}")
        });
        format!("Source Name Disk Size Accessed State\n{}\n", rows.join("\n"))
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
